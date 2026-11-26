use axum::Router;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request};
use axum::routing::{delete, get, post, put};
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::AppState;
use crate::error::ApiError;
use crate::http::*;

pub fn router(state: AppState) -> Router {
    let log = TraceLayer::new_for_http()
        .make_span_with(|request: &Request| {
            // The route's pattern, because an address can hold an invite's token.
            let route = request.extensions().get::<MatchedPath>();
            let route = route.map_or("", MatchedPath::as_str);
            tracing::info_span!("request", method = %request.method(), route)
        })
        .on_response(DefaultOnResponse::new().level(Level::INFO));

    let team = "/teams/{team_id}";
    let project = "/teams/{team_id}/projects/{project_id}";
    let api = Router::new()
        .route("/session", get(sessions::show))
        .route("/sign-in/code", post(sessions::send_code))
        .route("/sign-in/verify", post(sessions::create))
        .route("/sign-out", post(sessions::delete))
        .route(
            "/config/mail",
            get(mail_settings::show)
                .put(mail_settings::update)
                .delete(mail_settings::delete),
        )
        .route("/config/mail/test", post(mail_settings::test))
        .route("/invites/{token}", get(invites::show))
        .route("/invites/{token}/accept", post(invites::accept))
        .route("/live", get(live::connect))
        .route("/tools", post(tools::call))
        .route("/machines", get(machines::list).post(machines::pair))
        .route("/machines/codes", post(machines::code))
        .route("/machines/connect", get(machines::connect))
        .route("/machines/{machine_id}", delete(machines::delete))
        .route(
            "/machines/{machine_id}/sign-in/{agent}",
            get(machines::sign_in),
        )
        .route("/shares/{token}", get(shares::show))
        .route("/shares/{token}/files/{file_id}", get(files::shared))
        .nest(
            team,
            Router::new()
                .route("/", get(teams::show).put(teams::update))
                .route("/invites", post(invites::create))
                .route("/invites/{invite_id}", delete(invites::delete))
                .route("/members/{user_id}", delete(teams::remove_member))
                .route("/writer", put(writer::update).delete(writer::delete))
                .route("/key", put(api_key::update).delete(api_key::delete))
                .route(
                    "/github",
                    get(github::show)
                        .post(github::create)
                        .delete(github::delete),
                )
                .route("/github/branches", get(github::branches))
                .route("/projects", get(projects::list).post(projects::create))
                .route(
                    "/projects/{project_id}",
                    put(projects::update).delete(projects::delete),
                )
                .route("/groups", get(groups::list).post(groups::create))
                .route("/groups/{group_id}", get(groups::show).put(groups::update))
                .route("/groups/{group_id}/members", post(groups::add_member))
                .route("/groups/{group_id}/members/me", delete(groups::leave))
                .route("/groups/{group_id}/messages", post(groups::write))
                .route("/chats", get(chats::list))
                .route("/chats/{chat_id}", get(chats::show).delete(chats::delete))
                .route("/chats/{chat_id}/messages", post(chats::say))
                .route("/chats/{chat_id}/stop", post(chats::stop))
                .route("/chats/{chat_id}/diff", get(chats::diff))
                .route("/chats/{chat_id}/push", post(chats::push))
                .route("/chats/{chat_id}/hosts", post(chats::allow_host))
                .route("/chats/{chat_id}/hosts/{host}", delete(chats::forget_host)),
        )
        .nest(
            project,
            Router::new()
                .route("/map", get(maps::show))
                .route("/chats", post(chats::create))
                .route(
                    "/tasks",
                    get(tasks::list).post(tasks::create).put(tasks::order),
                )
                .route("/tasks/{task_id}", put(tasks::update).delete(tasks::delete))
                .route("/tasks/{task_id}/docs", get(tasks::docs))
                .route("/docs", get(docs::show).post(docs::write))
                .route("/docs/requests", post(docs::ask))
                .route("/docs/pages", post(pages::create).put(pages::order))
                .route(
                    "/docs/pages/{page_id}",
                    put(pages::update).delete(pages::delete),
                )
                .route("/docs/pages/{page_id}/versions", get(pages::versions))
                .route(
                    "/docs/pages/{page_id}/notes/{index}",
                    delete(pages::dismiss_note),
                )
                .route(
                    "/docs/proposals/{proposal_id}",
                    post(proposals::accept).delete(proposals::delete),
                )
                .route(
                    "/files",
                    // A little over the limit, so that a too large file gets our own message.
                    post(files::create).layer(DefaultBodyLimit::max(files::MAX_BYTES + 1024)),
                )
                .route("/files/{file_id}", get(files::show))
                .route("/shares", get(shares::list).post(shares::create))
                .route("/shares/{token}", delete(shares::delete)),
        )
        .fallback(async || ApiError::not_found())
        .layer(log)
        // Added after the layer, so health checks aren't logged.
        .route("/health", get(health::show))
        .with_state(state);
    Router::new().nest("/api", api).fallback(web::serve)
}
