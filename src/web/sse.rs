use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use tokio::sync::broadcast;

use crate::state::{BusEvent, SharedState};

pub async fn events(
    State(state): State<SharedState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.events.subscribe();
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let name = match &event {
                        BusEvent::Log { .. } => "log",
                        BusEvent::Device { .. } => "device",
                        BusEvent::Download { .. } => "download",
                    };
                    let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
                    return Some((Ok(Event::default().event(name).data(data)), rx));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
