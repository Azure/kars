use actix_web::{web, App, HttpServer, middleware};
use actix_files::Files;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use tracing::{info, warn, error};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod auth;
mod handlers;
mod models;
mod db;
mod oauth;
mod org;
mod revocation;
mod reputation;
mod certs;
mod state;

pub use state::AppState;

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "agentmesh_registry=info,actix_web=info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Database connection - use connect_lazy for immediate return
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://localhost/agentmesh".to_string());

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect_lazy(&database_url)?;  // Returns immediately, connections made on demand

    // Create shared application state (not ready yet)
    let app_state = Arc::new(AppState::new(pool.clone()));

    // Spawn background task for database initialization with retry logic
    let init_state = app_state.clone();
    let init_pool = pool.clone();
    tokio::spawn(async move {
        info!("Starting background database initialization...");

        // Retry configuration
        let max_attempts = 10;
        let initial_delay = std::time::Duration::from_secs(2);
        let max_delay = std::time::Duration::from_secs(30);
        let timeout_per_attempt = std::time::Duration::from_secs(10);

        let mut attempt = 0;
        let mut delay = initial_delay;

        loop {
            attempt += 1;
            info!("Database connection attempt {}/{}", attempt, max_attempts);

            // Try to acquire a connection with timeout
            let acquire_result = tokio::time::timeout(
                timeout_per_attempt,
                init_pool.acquire()
            ).await;

            match acquire_result {
                Ok(Ok(_conn)) => {
                    info!("Database connection established on attempt {}", attempt);

                    // Run migrations (handle checksum errors gracefully for development)
                    match sqlx::migrate!("./migrations").run(&init_pool).await {
                        Ok(_) => {
                            info!("Database migrations completed successfully");
                            init_state.set_ready();
                            info!("Application ready to serve requests");
                            return;
                        }
                        Err(e) => {
                            let err_msg = e.to_string();
                            if err_msg.contains("previously applied but has been modified") {
                                warn!("Migration checksum mismatch detected - continuing with existing schema: {}", err_msg);
                                init_state.set_ready();
                                info!("Application ready to serve requests");
                                return;
                            } else {
                                error!("Migration failed: {}. Will retry...", e);
                            }
                        }
                    }
                }
                Ok(Err(e)) => {
                    warn!("Database connection attempt {} failed: {}", attempt, e);
                }
                Err(_) => {
                    warn!("Database connection attempt {} timed out after {:?}", attempt, timeout_per_attempt);
                }
            }

            // Check if we've exhausted retries
            if attempt >= max_attempts {
                error!("Failed to connect to database after {} attempts. Application will not become ready.", max_attempts);
                return;
            }

            // Wait before retrying with exponential backoff
            info!("Retrying in {:?}...", delay);
            tokio::time::sleep(delay).await;
            delay = std::cmp::min(delay * 2, max_delay);
        }
    });

    // Background cert expiry sweep — demotes agents with expired certificates
    // to anonymous tier. Runs every hour.
    let sweep_pool = pool.clone();
    let sweep_state = app_state.clone();
    tokio::spawn(async move {
        // Wait for DB to be ready before first sweep
        loop {
            if sweep_state.is_ready() { break; }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
        info!("Certificate expiry sweep task started (1hr interval)");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            match db::demote_expired_certs(&sweep_pool).await {
                Ok(0) => {}
                Ok(n) => info!("Cert expiry sweep: demoted {} agents to anonymous", n),
                Err(e) => warn!("Cert expiry sweep error: {}", e),
            }
        }
    });

    // Background stale agent cleanup — removes agents not seen in 7 days.
    // Search already filters at 5 minutes; this prevents DB bloat from
    // accumulating zombie entries. Dormant (handoff predecessors) are preserved.
    let cleanup_pool = pool.clone();
    let cleanup_state = app_state.clone();
    tokio::spawn(async move {
        loop {
            if cleanup_state.is_ready() { break; }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
        info!("Stale agent cleanup task started (6hr interval)");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            match db::cleanup_stale_agents(&cleanup_pool).await {
                Ok(0) => {}
                Ok(n) => info!("Stale cleanup: removed {} agents not seen in 7 days", n),
                Err(e) => warn!("Stale agent cleanup error: {}", e),
            }
        }
    });

    // Server config
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse()?;

    info!("Starting AgentMesh Registry on {}:{}", host, port);

    // Start server immediately (don't wait for DB)
    let server_state = app_state.clone();
    let server = HttpServer::new(move || {
        let mut app = App::new()
            .app_data(web::Data::new(server_state.clone()))
            .wrap(middleware::Logger::default())
            .wrap(middleware::Compress::default())
            // API routes first (higher priority)
            .configure(handlers::configure_routes);

        // Static files with SPA fallback — only if static dir exists
        // (avoids panic when deployed without frontend assets)
        if std::path::Path::new("./static/index.html").exists() {
            app = app.service(
                Files::new("/", "./static")
                    .index_file("index.html")
                    .default_handler(
                        actix_files::NamedFile::open("./static/index.html")
                            .expect("checked above")
                    )
            );
        }
        app
    })
    .bind((host.as_str(), port))?
    .run();

    // Graceful shutdown: listen for SIGTERM/Ctrl+C, then drain in-flight requests
    let handle = server.handle();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Shutdown signal received, draining connections...");
        handle.stop(true).await;
    });

    server.await?;

    Ok(())
}
