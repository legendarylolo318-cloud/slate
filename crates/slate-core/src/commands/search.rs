use crate::tidal_api::{SuggestionsResponse, TidalSearchResults};
use crate::AppState;
use crate::SlateError;

pub async fn search_tidal(
    state: &AppState,
    query: String,
    limit: u32,
) -> Result<TidalSearchResults, SlateError> {
    log::debug!("[search_tidal]: query=\"{}\", limit={}", query, limit);
    let mut client = state.tidal_client.lock().await;
    client.search(&query, limit).await
}

pub async fn get_suggestions(
    state: &AppState,
    query: String,
    limit: u32,
) -> Result<SuggestionsResponse, SlateError> {
    log::debug!("[get_suggestions]: query=\"{}\", limit={}", query, limit);
    let mut client = state.tidal_client.lock().await;
    Ok(client.get_suggestions(&query, limit).await)
}
