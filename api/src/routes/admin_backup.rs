use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::fs::File as TokioFile;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::backup;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, BackupJobStatus};

pub fn router(backup_upload_max_mb: usize) -> Router<AppState> {
    let upload_limit = backup_upload_max_mb.saturating_mul(1024 * 1024).max(1024 * 1024);
    Router::new()
        .route("/backups", get(list_backups).post(create_backup_job))
        .route(
            "/backups/upload",
            post(upload_backup).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route("/backups/jobs/{id}", get(get_job))
        .route("/backups/{name}/download", get(download_backup))
        .route("/backups/{name}/restore", post(restore_backup_job))
        .route("/backups/{name}", axum::routing::delete(delete_backup))
}

async fn list_backups(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let items = backup::list_backups(&state.cfg)?;
    Ok(Json(json!({ "items": items })))
}

async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let job = state.get_job(&id).ok_or_else(ApiError::not_found)?;
    Ok(Json(json!({
        "status": job.status,
        "error": job.error,
        "backup_name": job.backup_name,
    })))
}

fn spawn_job<F, Fut>(state: AppState, job_id: String, work: F)
where
    F: FnOnce(AppState) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ApiResult<Option<String>>> + Send,
{
    state.set_job(
        &job_id,
        BackupJobStatus {
            status: "running".into(),
            error: None,
            backup_name: None,
        },
    );
    tokio::spawn(async move {
        let lock = state.backup_lock.try_lock();
        let result = match lock {
            Ok(_guard) => work(state.clone()).await,
            Err(_) => Err(ApiError::new(
                StatusCode::CONFLICT,
                "已有备份或恢复任务进行中，请稍后再试",
            )),
        };
        match result {
            Ok(name) => {
                state.set_job(
                    &job_id,
                    BackupJobStatus {
                        status: "done".into(),
                        error: None,
                        backup_name: name,
                    },
                );
            }
            Err(err) => {
                state.set_job(
                    &job_id,
                    BackupJobStatus {
                        status: "error".into(),
                        error: Some(err.message),
                        backup_name: None,
                    },
                );
            }
        }
    });
}

async fn create_backup_job(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let job_id = Uuid::new_v4().to_string();
    state.set_job(
        &job_id,
        BackupJobStatus {
            status: "pending".into(),
            error: None,
            backup_name: None,
        },
    );
    spawn_job(state, job_id.clone(), |state| async move {
        let name = backup::create_backup(&state).await?;
        Ok(Some(name))
    });
    Ok(Json(json!({ "job_id": job_id })))
}

async fn restore_backup_job(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<impl IntoResponse> {
    if !backup::is_valid_backup_name(&name) {
        return Err(ApiError::bad_request("invalid backup filename"));
    }
    let path = backup::backup_path(&state.cfg, &name)?;
    if !path.exists() {
        return Err(ApiError::not_found());
    }
    let job_id = Uuid::new_v4().to_string();
    state.set_job(
        &job_id,
        BackupJobStatus {
            status: "pending".into(),
            error: None,
            backup_name: None,
        },
    );
    let name_c = name.clone();
    spawn_job(state, job_id.clone(), move |state| async move {
        backup::restore_backup(&state, &name_c).await?;
        Ok(Some(name_c))
    });
    Ok(Json(json!({ "job_id": job_id })))
}

async fn upload_backup(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> ApiResult<impl IntoResponse> {
    let lock = state.backup_lock.try_lock();
    let _guard = lock.map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "已有备份或恢复任务进行中，请稍后再试",
        )
    })?;

    let mut saved: Option<PathBuf> = None;
    let mut total: usize = 0;
    let max_bytes = state.cfg.backup_upload_max_mb.saturating_mul(1024 * 1024);

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("multipart: {e}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let filename = field.file_name().unwrap_or("upload.zip").to_string();
        if !filename.to_ascii_lowercase().ends_with(".zip") {
            return Err(ApiError::bad_request("仅支持 .zip 备份文件"));
        }

        let stamp = chrono::Utc::now().format("%Y%m%d_%H%M%S_%f");
        let tmp = state.cfg.backup_dir.join(format!(".upload_{stamp}.zip"));
        let mut file = TokioFile::create(&tmp)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;

        let mut field = field;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| ApiError::bad_request(format!("read upload: {e}")))?
        {
            total = total.saturating_add(chunk.len());
            if total > max_bytes {
                drop(file);
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(ApiError::bad_request(format!(
                    "上传超过上限 {} MB",
                    state.cfg.backup_upload_max_mb
                )));
            }
            file.write_all(&chunk)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?;
        }
        file.flush()
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
        saved = Some(tmp);
        break;
    }

    let tmp = saved.ok_or_else(|| ApiError::bad_request("missing file field"))?;
    let name = backup::validate_uploaded_backup(&state.cfg, &tmp)?;
    Ok(Json(json!({ "name": name })))
}

async fn download_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Response> {
    let path = backup::backup_path(&state.cfg, &name)?;
    if !path.exists() {
        return Err(ApiError::not_found());
    }
    let file = TokioFile::open(&path)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);
    let disposition = format!("attachment; filename=\"{name}\"");
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(body)
        .map_err(|e| ApiError::internal(e.to_string()))?)
}

async fn delete_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<impl IntoResponse> {
    backup::delete_backup(&state.cfg, &name)?;
    Ok(StatusCode::NO_CONTENT)
}
