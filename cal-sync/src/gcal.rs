use std::{fs, path::PathBuf};

use axum::{
    extract::{Query, State},
    routing::get,
    Router,
};
use color_eyre::{
    eyre::{eyre, ContextCompat, OptionExt},
    Result,
};
use futures::future::join_all;
use google_calendar::{
    types::{Event, MinAccessRole, OrderBy, SendUpdates},
    AccessToken, Client,
};
use serde::Deserialize;
use tokio::sync::{
    mpsc,
    oneshot::{self},
};
use tracing::{debug, info};

use crate::org::AgendaItem;

const PORT: u16 = 8081;
const TIMEOUT: u64 = 90;

#[derive(Debug, Deserialize)]
struct Credentials {
    installed: CredentialsInner,
}

#[derive(Debug, Deserialize)]
struct CredentialsInner {
    client_id: String,
    client_secret: String,
}

pub async fn sync(client: Client, events: Vec<AgendaItem>, calendar_summary: &str) -> Result<()> {
    // First, delete all the events with the matching description.
    const GENERATED_DESC: &str = "cal_sync.py marker description";

    // First, let's get the calendar ID.
    let cal = client
        .calendar_list()
        .list_all(MinAccessRole::Noop, false, false)
        .await?
        .body
        .into_iter()
        .find(|c| c.summary == calendar_summary)
        .wrap_err(format!("Couldn't find calendar {}", calendar_summary))?;

    // Next, find all events in this calendar with the matching description.
    let dels = join_all(
        client
            .events()
            .list_all(
                &cal.id,
                "",
                0,
                OrderBy::Noop,
                &[],
                "",
                &[],
                false,
                false,
                false,
                "",
                "",
                "",
                "",
            )
            .await?
            .body
            .into_iter()
            .filter(|ev| ev.description == GENERATED_DESC)
            .map(|ev| {
                let cal_id = cal.id.clone();
                let client = client.clone();

                async move {
                    let ev_id = ev.id;

                    debug!("del {}", ev.summary);

                    client
                        .events()
                        .delete(&cal_id, &ev_id, false, SendUpdates::Noop)
                        .await
                }
            }),
    )
    .await;

    // Await all delete tasks
    let mut deleted_evs = 0;
    for res in dels {
        let _ = res?;
        deleted_evs += 1;
    }
    info!("Deleted: {deleted_evs}");

    // Now, let's add all of our org tasks
    let adds = join_all(
        events
            .into_iter()
            .flat_map(|new_ev| {
                new_ev
                    .timestamps
                    .into_iter()
                    .map(move |x| (x, new_ev.name.clone()))
            })
            .map(|(s, name)| {
                let client = client.clone();
                let cal_id = cal.id.clone();

                let (start, end, rep) = s.into_gcal();

                async move {
                    let e = Event {
                        summary: format!("TS: {name}"),
                        description: GENERATED_DESC.to_string(),
                        start: Some(start),
                        end,
                        recurrence: rep.map(|r| vec![r]).unwrap_or_else(Vec::new),
                        color_id: "8".to_string(),
                        ..Default::default()
                    };

                    client
                        .events()
                        .insert(&cal_id, 0, 0, false, SendUpdates::Noop, false, &e)
                        .await
                }
            }),
    )
    .await;

    // Await all insert tasks
    let mut inserted_evs = 0;
    for res in adds {
        let r = res?;
        debug!("ins {}", r.body.summary);
        inserted_evs += 1;
    }
    info!("Inserted: {inserted_evs}");

    println!("-{deleted_evs} +{inserted_evs}");

    Ok(())
}

async fn try_refresh_client(
    client_id: String,
    client_secret: String,
    redirect_uri: &str,
    token_path: PathBuf,
) -> Result<Client> {
    if !token_path.exists() {
        return Err(eyre!("w/e"));
    }
    let data = fs::read_to_string(&token_path)?;
    let tok = serde_json::from_str::<'_, AccessToken>(&data)?;

    let c = Client::new(
        client_id,
        client_secret,
        redirect_uri,
        tok.access_token,
        tok.refresh_token,
    );

    let new_tok = c.refresh_access_token().await?;

    // Write our new token
    let out = serde_json::to_string_pretty(&new_tok)?;
    fs::write(token_path, out)?;

    Ok(c)
}

pub async fn get_client(creds_path: PathBuf, token_path: PathBuf) -> Result<Client> {
    let scopes: [String; 2] = [
        "https://www.googleapis.com/auth/calendar.readonly".to_string(),
        "https://www.googleapis.com/auth/calendar.events".to_string(),
    ];
    let creds = fs::read_to_string(creds_path)?;
    let Credentials {
        installed: CredentialsInner {
            client_id,
            client_secret,
        },
    } = serde_json::from_str::<'_, Credentials>(&creds)?;

    let redirect_uri = format!("http://localhost:{}", PORT);

    if let Ok(c) = try_refresh_client(
        client_id.clone(),
        client_secret.clone(),
        &redirect_uri,
        token_path.clone(),
    )
    .await
    {
        Ok(c)
    } else {
        let mut c = Client::new(client_id, client_secret, &redirect_uri, "", "");

        let (resp_tx, resp_rx) = oneshot::channel();

        tokio::spawn(spawn_oauth_listener(resp_tx));

        // Get the URL to request consent from the user.
        // You can optionally pass in scopes. If none are provided, then the
        // resulting URL will not have any scopes.
        let user_consent_url = c.user_consent_url(&scopes);
        open::that(user_consent_url)?;

        // In your redirect URL capture the code sent and our state.
        // Send it along to the request for the token.
        let OAuthResp { code, state } = resp_rx.await?.ok_or_eyre("Timed out.")?;
        let access_token = c.get_access_token(&code, &state).await?;

        // Write the access token back
        let new_state = serde_json::to_string_pretty(&access_token)?;
        fs::write(token_path, new_state)?;

        Ok(c)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct OAuthResp {
    code: String,
    state: String,
}

async fn spawn_oauth_listener(tx: oneshot::Sender<Option<OAuthResp>>) -> Result<()> {
    // Creating an OAuth listener requires that we have a channel to send the OAuth response from.
    // We also create another channel that we use to kill this server once we've received the
    // correct response.
    // We use mpsc and not oneshot because there's no way of telling axum that our route is only
    // ever called once.
    let (inner_tx, mut inner_rx) = mpsc::channel::<OAuthResp>(1);

    // One sends information from
    let app = Router::new()
        .route(
            "/",
            get(
                move |State(state): State<mpsc::Sender<OAuthResp>>, resp: Query<OAuthResp>| async move {
                    // Send the resp over the channel
                    state
                        .send(resp.0)
                        .await
                        .expect("Couldn't send OAuthResp in server");

                    // Reply with text
                    "Successfully received OAuth response. You can close this window."
                },
            ),
        )
        .with_state(inner_tx);

    // run our app with hyper, listening globally on port PORT
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", PORT))
        .await
        .unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let value = async { inner_rx.recv().await.expect("Couldn't get inner_rx value") };
            let timer = tokio::time::sleep(std::time::Duration::from_secs(TIMEOUT));

            tokio::select! {
                _ = timer => {
                    tx.send(None).expect("Couldn't send");
                },
                v = value => {

                    tx.send(Some(v)).expect("Couldn't send");

                },
            }
        })
        .await
        .unwrap();

    Ok(())
}
