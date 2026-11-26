use axum::Router;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request};
use axum::middleware::from_fn_with_state;
use axum::routing::{delete, get, post, put};
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::auth::team_member;
use crate::error::ApiError;
use crate::{AppState, accounts, chats, docs, groups, live, machines, projects, settings, tasks, web};

pub fn router(state: AppState) -> Router {
    let log = TraceLayer::new_for_http()
        .make_span_with(|request: &Request| {
            // The route's pattern, because an address can hold an invite's token.
            let route = request.extensions().get::<MatchedPath>().map_or("", MatchedPath::as_str);
            tracing::info_span!("request", method = %request.method(), route)
        })
        .on_response(DefaultOnResponse::new().level(Level::INFO));

    let team = "/teams/{team_id}";
    let project = "/teams/{team_id}/projects/{project_id}";
    let api = Router::new()
        .route("/session", get(accounts::http::show_session))
        .route("/sign-in/code", post(accounts::http::send_code))
        .route("/sign-in/verify", post(accounts::http::create_session))
        .route("/sign-out", post(accounts::http::delete_session))
        .route(
            "/config/mail",
            get(settings::http::show_mail)
                .put(settings::http::update_mail)
                .delete(settings::http::delete_mail),
        )
        .route("/config/mail/test", post(settings::http::test_mail))
        .route("/invites/{token}", get(accounts::http::show_invite))
        .route("/invites/{token}/accept", post(accounts::http::accept_invite))
        .route("/live", get(live::connect))
        .route("/tools", post(chats::http::call_tool))
        .route("/machines", get(machines::http::list_machines).post(machines::http::pair))
        .route("/machines/codes", post(machines::http::code))
        .route("/machines/connect", get(machines::http::connect))
        .route("/machines/{machine_id}", delete(machines::http::delete_machine))
        .route("/machines/{machine_id}/sign-in/{agent}", get(machines::http::sign_in))
        .route("/shares/{token}", get(docs::http::show_share))
        .route("/shares/{token}/files/{file_id}", get(docs::http::shared_file))
        .nest(
            team,
            Router::new()
                .route("/", get(accounts::http::show_team).put(accounts::http::update_team))
                .route("/invites", post(accounts::http::create_invites))
                .route("/invites/{invite_id}", delete(accounts::http::delete_invite))
                .route("/members/{user_id}", delete(accounts::http::remove_member))
                .route("/writer", put(settings::http::update_writer).delete(settings::http::delete_writer))
                .route("/key", put(settings::http::update_api_key).delete(settings::http::delete_api_key))
                .route(
                    "/github",
                    get(projects::http::show_github)
                        .post(projects::http::create_github)
                        .delete(projects::http::delete_github),
                )
                .route("/github/branches", get(projects::http::repo_branches))
                .route("/projects", get(projects::http::list_projects).post(projects::http::create_project))
                .route(
                    "/projects/{project_id}",
                    put(projects::http::update_project).delete(projects::http::delete_project),
                )
                .route("/groups", get(groups::http::list_groups).post(groups::http::create_group))
                .route("/groups/{group_id}", get(groups::http::show_group).put(groups::http::update_group))
                .route("/groups/{group_id}/members", post(groups::http::add_member))
                .route("/groups/{group_id}/members/me", delete(groups::http::leave))
                .route("/groups/{group_id}/messages", post(groups::http::write))
                .route("/chats", get(chats::http::list_chats))
                .route("/chats/{chat_id}", get(chats::http::show_chat).delete(chats::http::delete_chat))
                .route("/chats/{chat_id}/messages", post(chats::http::say))
                .route("/chats/{chat_id}/stop", post(chats::http::stop))
                .route("/chats/{chat_id}/diff", get(chats::http::diff))
                .route("/chats/{chat_id}/push", post(chats::http::push))
                .route("/chats/{chat_id}/hosts", post(chats::http::allow_host))
                .route("/chats/{chat_id}/hosts/{host}", delete(chats::http::forget_host))
                .route_layer(from_fn_with_state(state.clone(), team_member)),
        )
        .nest(
            project,
            Router::new()
                .route("/map", get(projects::http::show_map))
                .route("/chats", post(chats::http::create_chat))
                .route(
                    "/tasks",
                    get(tasks::http::list_tasks).post(tasks::http::create_task).put(tasks::http::order_tasks),
                )
                .route("/tasks/{task_id}", put(tasks::http::update_task).delete(tasks::http::delete_task))
                .route("/tasks/{task_id}/docs", get(tasks::http::suggested_docs))
                .route("/docs", get(docs::http::show_docs).post(docs::http::write_docs))
                .route("/docs/requests", post(docs::http::ask_docs))
                .route("/docs/pages", post(docs::http::create_page).put(docs::http::order_pages))
                .route("/docs/pages/{page_id}", put(docs::http::update_page).delete(docs::http::delete_page))
                .route("/docs/pages/{page_id}/versions", get(docs::http::page_versions))
                .route("/docs/pages/{page_id}/notes/{index}", delete(docs::http::dismiss_note))
                .route(
                    "/docs/proposals/{proposal_id}",
                    post(docs::http::accept_proposal).delete(docs::http::delete_proposal),
                )
                .route(
                    "/files",
                    // A little over the limit, so that a too large file gets our own message.
                    post(docs::http::upload_file).layer(DefaultBodyLimit::max(docs::http::MAX_BYTES + 1024)),
                )
                .route("/files/{file_id}", get(docs::http::show_file))
                .route("/shares", get(docs::http::list_shares).post(docs::http::create_share))
                .route("/shares/{token}", delete(docs::http::delete_share))
                .route_layer(from_fn_with_state(state.clone(), team_member)),
        )
        .fallback(async || ApiError::not_found())
        .layer(log)
        .route("/health", get(web::health))
        .with_state(state);
    Router::new().nest("/api", api).fallback(web::serve_web)
}
