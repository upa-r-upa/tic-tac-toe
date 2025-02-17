use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, mpsc};
use std::sync::Arc;
use tonic::Request;
use std::time::Duration;

use tictactoe::tic_tac_toe_client::TicTacToeClient;
use tictactoe::{GameState, Move};

pub mod tictactoe {
    tonic::include_proto!("tictactoe");
}

/// 클라이언트의 공유 상태 구조체
struct ClientState {
    // 할당된 플레이어 심볼 ("X" 또는 "O")
    player_symbol: Mutex<Option<String>>,
    // 현재 게임 상태 (예: "waiting", "ongoing", "X_win", ...)
    game_status: Mutex<String>,
    // 게임 종료 여부 (true면 더 이상 입력을 받지 않음)
    game_over: Mutex<bool>,
}

impl ClientState {
    fn new() -> Self {
        ClientState {
            player_symbol: Mutex::new(None),
            game_status: Mutex::new(String::new()),
            game_over: Mutex::new(false),
        }
    }
}

/// 보드 출력 함수
fn print_board(board: &Vec<String>) {
    println!("-------------");
    for i in 0..3 {
        println!("| {} | {} | {} |", board[i * 3], board[i * 3 + 1], board[i * 3 + 2]);
        println!("-------------");
    }
}

/// 서버 업데이트 처리 함수
async fn process_server_updates(mut rx: tonic::Streaming<GameState>, state: Arc<ClientState>) {
    while let Some(result) = rx.message().await.unwrap_or(None) {
        {
            let mut status = state.game_status.lock().await;
            *status = result.status.clone();
        }

        println!("\n=== Game Update ===");

        if !result.error_message.is_empty() {
            println!("Error: {}", result.error_message);
        }

        match result.status.as_str() {
            "waiting" => {
                let symbol = state.player_symbol.lock().await.clone();
                if symbol.is_some() {
                    println!("Opponent disconnected. Waiting for opponent to join...");
                } else {
                    println!("Waiting for opponent to join...");
                }
            },
            "ongoing" => {
                print_board(&result.board);
                println!("Next Player: {}", result.next_player);
                println!("Your Symbol: {}", result.your_symbol);
            },
            "X_win" | "O_win" | "draw" => {
                print_board(&result.board);
                println!("Game Over: {}", result.status);
                let mut over = state.game_over.lock().await;
                *over = true;
                break;
            },
            _ => {
                println!("Status: {}", result.status);
            }
        }

        {
            let mut sym_lock = state.player_symbol.lock().await;
            if sym_lock.is_none() || sym_lock.as_ref().unwrap() != &result.your_symbol {
                *sym_lock = Some(result.your_symbol.clone());
                println!("Your symbol has been updated to: {}", result.your_symbol);
            }
        }

        println!("===================\n");
    }
    println!("Disconnected from server.");
    let mut over = state.game_over.lock().await;
    *over = true;
}

/// 사용자 입력 처리 함수 (자동 종료를 위해 select! 사용)
async fn process_user_input(move_tx: mpsc::Sender<Move>, state: Arc<ClientState>) {
    let stdin = io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    println!("Enter your move (0-8), or type 'exit' to quit:");
    loop {
        tokio::select! {
            maybe_line = lines.next_line() => {
                match maybe_line {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.eq_ignore_ascii_case("exit") {
                            break;
                        }
                        {
                            let over = state.game_over.lock().await;
                            if *over {
                                println!("Game is over. No more moves accepted.");
                                break;
                            }
                        }
                        {
                            let status = state.game_status.lock().await;
                            if *status == "waiting" {
                                println!("Game has not started yet. Waiting for opponent...");
                                continue;
                            }
                        }
                        if let Ok(pos) = trimmed.parse::<usize>() {
                            if pos < 9 {
                                let symbol_opt = {
                                    let lock = state.player_symbol.lock().await;
                                    lock.clone()
                                };
                                if let Some(symbol) = symbol_opt {
                                    let mv = Move {
                                        player_id: symbol.clone(),
                                        position: pos as i32,
                                    };
                                    if let Err(e) = move_tx.send(mv).await {
                                        eprintln!("Error sending move: {:?}", e);
                                    }
                                } else {
                                    println!("You haven't been assigned a symbol yet. Please wait for the server update.");
                                }
                            } else {
                                println!("Invalid move. Please enter a number between 0 and 8.");
                            }
                        } else {
                            println!("Invalid input. Please enter a number between 0 and 8, or 'exit'.");
                        }
                    },
                    Ok(None) => {
                        // EOF
                        break;
                    },
                    Err(e) => {
                        eprintln!("Error reading input: {:?}", e);
                        break;
                    }
                }
            },
            _ = async {
                loop {
                    {
                        let over = state.game_over.lock().await;
                        if *over {
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            } => {
                println!("Game session ended due to disconnection.");
                break;
            }
        }
    }
    println!("Exiting game session.");
}

/// 메인 게임 실행 함수
async fn run_game() -> Result<(), Box<dyn std::error::Error>> {
    println!("Connecting to gRPC server...");
    let mut client = TicTacToeClient::connect("http://[::1]:50051").await?;

    let (move_tx, move_rx) = mpsc::channel(32);
    let outbound = tokio_stream::wrappers::ReceiverStream::new(move_rx);

    let response = client.play(Request::new(outbound)).await?;
    let (_metadata, rx, _extensions) = response.into_parts();

    let client_state = Arc::new(ClientState::new());

    let state_clone = Arc::clone(&client_state);
    tokio::spawn(async move {
        process_server_updates(rx, state_clone).await;
    });

    process_user_input(move_tx, client_state).await;

    Ok(())
}

/// 메인 함수: 게임 종료 후 터미널 종료
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Err(e) = run_game().await {
        eprintln!("Error in game session: {:?}", e);
    }
    println!("Game session ended. Exiting.");
    Ok(())
}
