use std::time::Instant;

use actix_web::{get, web::Data, HttpResponse, Responder, Result};
use serde::Serialize;
use tracing::error;

use crate::service::{PulseService, Stats};

#[derive(Serialize)]
struct StatsResponse {
    stats: Stats,
    speed: f64,
}

#[get("/stats")]
pub async fn stats(service: Data<PulseService>) -> Result<impl Responder> {
    let start = Instant::now();
    let stats = service.get_stats();
    let elapsed = start.elapsed().as_micros() as f64 / 1000.0;

    if let Err(e) = stats {
        error!("Failed to get stats: {:?}", e);
        return Ok(HttpResponse::InternalServerError().finish());
    }

    let stats = stats.unwrap();

    let response = StatsResponse {
        stats,
        speed: elapsed,
    };

    Ok(HttpResponse::Ok().json(response))
}