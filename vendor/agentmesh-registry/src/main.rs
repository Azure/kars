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

    // Server config
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse()?;

    info!("Starting AgentMesh Registry on {}:{}", host, port);

    // Start server immediately (don't wait for DB)
    let server_state = app_state.clone();
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(server_state.clone()))
            .wrap(middleware::Logger::default())
            .wrap(middleware::Compress::default())
            // API routes first (higher priority)
            .configure(handlers::configure_routes)
            // Static files with SPA fallback (lower priority)
            // Serves from ./static directory, falls back to index.html for client-side routing
            .service(
                Files::new("/", "./static")
                    .index_file("index.html")
                    .default_handler(
                        actix_files::NamedFile::open("./static/index.html")
                            .expect("index.html should exist in ./static directory")
                    )
            )
    })
    .bind((host.as_str(), port))?
    .run()
    .await?;

    Ok(())
}
