/*
 * Copyright (C) 2026 ROS-Industrial Consortium Asia Pacific
 * Advanced Remanufacturing and Technology Centre
 * A*STAR Research Entities (Co. Registration No. 199702110H)
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

pub use crate::api::models::{TaskRequest, TaskResponse, TaskStatus};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use dashmap::DashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub task_store: Arc<DashMap<String, TaskResponse>>,
    pub executor_url: String,
}

#[utoipa::path(
    post,
    path = "/api/tasks",
    tag = "task",
    request_body = TaskRequest,
    responses(
        (status = 202, description = "Task accepted; poll GET /api/tasks/{task_id} for result", body = TaskResponse),
        (status = 422, description = "Unprocessable request"),
    )
)]
pub async fn create_task(
    State(state): State<AppState>,
    Json(req): Json<TaskRequest>,
) -> impl IntoResponse {
    let task_id = req.id.clone();
    let record = TaskResponse {
        task_id: task_id.clone(),
        status: TaskStatus::Pending,
        result: None,
        error: None,
    };
    state.task_store.insert(task_id.clone(), record.clone());

    let store = state.task_store.clone();
    let executor_url = state.executor_url.clone();
    tokio::spawn(async move {
        run_task(store, executor_url, task_id, req).await;
    });

    (StatusCode::ACCEPTED, Json(record))
}

#[utoipa::path(
    get,
    path = "/api/tasks",
    tag = "task",
    responses(
        (status = 200, description = "All known task statuses", body = Vec<TaskResponse>),
    )
)]
pub async fn list_tasks(State(state): State<AppState>) -> Json<Vec<TaskResponse>> {
    Json(state.task_store.iter().map(|r| r.value().clone()).collect())
}

#[utoipa::path(
    get,
    path = "/api/tasks/{task_id}",
    tag = "task",
    params(
        ("task_id" = String, Path, description = "Task ID returned by POST /api/tasks")
    ),
    responses(
        (status = 200, description = "Task status", body = TaskResponse),
        (status = 404, description = "Task not found"),
    )
)]
pub async fn get_task_status(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> impl IntoResponse {
    match state.task_store.get(&task_id) {
        Some(record) => (StatusCode::OK, Json(record.clone())).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn run_task(
    store: Arc<DashMap<String, TaskResponse>>,
    executor_url: String,
    task_id: String,
    req: TaskRequest,
) {
    if let Some(mut r) = store.get_mut(&task_id) {
        r.status = TaskStatus::Running;
    }

    let diagram_json: serde_json::Value = match &req.payload {
        Some(p) if p.is_string() => {
            serde_json::from_str(p.as_str().unwrap()).unwrap_or_else(|_| p.clone())
        }
        Some(p) => p.clone(),
        None => serde_json::Value::Null,
    };

    let body = serde_json::json!({
        "diagram": diagram_json,
        "request": {
            "id": req.id,
            "task_type": req.task_type,
            "task_command": req.task_command,
            "asset_id": req.asset_id,
            "task_params": req.task_params,
        },
    });

    match reqwest::Client::new()
        .post(format!("{executor_url}/api/executor/run"))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let result = resp.json::<serde_json::Value>().await.ok();
            if let Some(mut r) = store.get_mut(&task_id) {
                r.status = TaskStatus::Completed;
                r.result = result;
            }
        }
        Ok(resp) => {
            let code = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if let Some(mut r) = store.get_mut(&task_id) {
                r.status = TaskStatus::Failed;
                r.error = Some(format!("{code}: {text}"));
            }
        }
        Err(e) => {
            if let Some(mut r) = store.get_mut(&task_id) {
                r.status = TaskStatus::Failed;
                r.error = Some(e.to_string());
            }
        }
    }
}
