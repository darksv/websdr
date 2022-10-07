#![feature(array_chunks)]

use std::{net::SocketAddr, path::PathBuf};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_channel::{Receiver, Sender};
use axum::{
    extract::{
        TypedHeader,
        ws::{self, Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    Router,
    routing::{get, get_service},
};
use axum::extract::State;
use futures_util::{SinkExt, StreamExt};
use futures_util::stream::{SplitSink, SplitStream};
use rtlsdr_rs::{DEFAULT_BUF_LENGTH, RtlSdr};
use rustfft::num_complex::{Complex, Complex32};
use rustfft::num_traits::Zero;
use serde::Serialize;
use tower_http::{
    services::ServeDir,
    trace::{DefaultMakeSpan, TraceLayer},
};
use tracing::{debug, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

struct ControllerState {
    data_rx: Receiver<Box<[u8]>>,
    cmd_tx: Sender<SdrCommand>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "tower_http=debug,websdr=debug".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let assets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");

    let (tx, rx) = async_channel::bounded(10);
    let (cmd_tx, cmd_rx) = async_channel::bounded(1);
    tokio::task::spawn_blocking(move || {
        let term = AtomicBool::new(false);
        sdr_worker(tx, cmd_rx, &term).unwrap();
    });

    let state = Arc::new(ControllerState {
        data_rx: rx,
        cmd_tx,
    });

    // build our application with some routes
    let app = Router::with_state(state)
        .fallback_service(
            get_service(ServeDir::new(assets_dir).append_index_html_on_directories(true))
                .handle_error(|error: std::io::Error| async move {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Unhandled internal error: {}", error),
                    )
                }),
        )
        // routes are matched from bottom to top, so we have to put `nest` at the
        // top since it matches all routes
        .route("/ws", get(ws_handler))
        // logging so we can see whats going on
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        );


    // run it with hyper
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    tracing::debug!("listening on {}", addr);
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    user_agent: Option<TypedHeader<headers::UserAgent>>,
    State(state): State<Arc<ControllerState>>,
) -> impl IntoResponse {
    if let Some(TypedHeader(user_agent)) = user_agent {
        info!("`{}` connected", user_agent.as_str());
    }

    ws.on_upgrade(move |socket| async {
        let (sink, stream) = socket.split();
        let (stats_tx, stats_rx) = async_channel::bounded(1);
        tokio::join!(handle_commands(stream, stats_tx, state.clone()), handle_data(sink, stats_rx, state));
        info!("Close");
    })
}

#[derive(Serialize)]
#[serde(untagged)]
#[serde(rename_all="snake_case")]
enum Stats {
    Frequency(u32),
}

async fn handle_data(mut socket: SplitSink<WebSocket, Message>, stats_rx: Receiver<Stats>, state: Arc<ControllerState>) {
    loop {
        if let Ok(fft) = state.data_rx.recv().await {
            if let Err(e) = socket.send(Message::Binary(fft.into_vec())).await {
                info!("client disconnected {:?}", e);
                return;
            }
        }

        if let Ok(stats) = stats_rx.try_recv() {
            if let Err(e) = socket.send(dbg!(Message::Text(serde_json::to_string(&stats).unwrap()))).await {
                info!("client disconnected {:?}", e);
                return;
            }
        }
    }
}

async fn handle_commands(mut stream: SplitStream<WebSocket>, stats_tx: Sender<Stats>, state: Arc<ControllerState>) {
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(msg) => match msg {
                Message::Text(payload) => {
                    let f = payload.parse().unwrap();
                    state.cmd_tx.send(SdrCommand::ChangeFrequency(f)).await.unwrap();
                    stats_tx.send(Stats::Frequency(f)).await.unwrap();
                }
                other => {
                    debug!("message: {:?}", &other);
                }
            },
            Err(e) => {
                info!("client disconnected: {:?}", e);
                return;
            }
        }
    }
}

enum SdrCommand {
    ChangeFrequency(u32),
}

const FFT_LEN: usize = 4096 * 2;

fn sdr_worker(tx: Sender<Box<[u8]>>, rx: Receiver<SdrCommand>, terminated: &AtomicBool) -> rtlsdr_rs::error::Result<()> {
    let mut sdr = RtlSdr::open(0).expect("Failed to open device");
    sdr.set_tuner_gain(rtlsdr_rs::TunerGain::Auto)?;
    sdr.set_bias_tee(false)?;
    sdr.reset_buffer()?;
    sdr.set_center_freq(95_000_000)?;
    sdr.set_sample_rate(1_200_000)?;

    info!("Tuned to {} Hz.\n", sdr.get_center_freq());
    // info!("Buffer size: {}ms", 1000.0 * 0.5 * DEFAULT_BUF_LENGTH as f32 / radio_config.capture_rate as f32);
    info!("Sampling at {} S/s", sdr.get_sample_rate());

    info!("Reading samples in sync mode...");

    let fft = rustfft::FftPlanner::new().plan_fft_forward(FFT_LEN);
    let mut scratch: Vec<Complex32> = vec![Complex32::zero(); fft.get_inplace_scratch_len()];
    let mut incoming_samples: Vec<Complex32> = Vec::new();
    let mut buffer: Vec<Complex32> = vec![Complex32::zero(); fft.len()];

    let window = make_hamming_window(FFT_LEN, 1.0);

    loop {
        if terminated.load(Ordering::Relaxed) {
            break;
        }

        while incoming_samples.len() >= FFT_LEN {
            buffer.clear();
            buffer.extend(incoming_samples[..FFT_LEN].iter().copied().zip(window.iter().copied()).map(|(a, b)| a * b));
            fft.process_with_scratch(&mut buffer[..FFT_LEN], &mut scratch);
            let frame: Vec<_> = buffer[..FFT_LEN / 2].iter().copied().map(|it| ((it.norm() as f32) * 4.0) as u8).collect();
            tx.send_blocking(frame.into_boxed_slice()).unwrap();
            incoming_samples.drain(..FFT_LEN);
        }

        if let Ok(command) = rx.try_recv() {
            match command {
                SdrCommand::ChangeFrequency(f) => {
                    if let Err(e) = sdr.reset_buffer() {
                        warn!("reset_buffer: {:?}", e);
                    }
                    if let Err(e) = sdr.set_center_freq(f) {
                        warn!("set_center_freq: {:?}", e);
                    }
                }
            }
        }

        let mut frame = [0u8; DEFAULT_BUF_LENGTH];
        let n = sdr.read_sync(&mut frame).unwrap();
        assert_eq!(n, DEFAULT_BUF_LENGTH);
        let samples = &frame[..n];
        incoming_samples.extend(samples.array_chunks::<2>()
            .map(|&[re, im]| Complex32::new(
                (re as i32 - 127) as f32 / 128.0,
                (im as i32 - 127) as f32 / 128.0,
            ))
        );
    }
    sdr.close()?;
    Ok(())
}

fn make_hamming_window(size: usize, scale: f32) -> Box<[f32]> {
    let mut window = Vec::with_capacity(size);
    for i in 0..size {
        window.push(scale * (0.54 - 0.46 * f32::cos(2.0 * std::f32::consts::PI * (i as f32) / (size as f32 - 1.0))));
    }
    window.into_boxed_slice()
}