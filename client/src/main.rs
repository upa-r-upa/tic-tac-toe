use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use std::sync::Arc;
use tonic::transport::Channel;
use tonic::Request;
use tictactoe::tic_tac_toe_client::TicTacToeClient;
use tictactoe::{GameState, Move};
use tokio_stream::StreamExt;

pub mod tictactoe {
    tonic::include_proto!("tictactoe");
}

/// 터미널에 현재 보드를 출력하는 함수
fn print_board(board: &Vec<String>) {
    println!("-------------");
    for i in 0..3 {
        println!("| {} | {} | {} |", board[i * 3], board[i * 3 + 1], board[i * 3 + 2]);
        println!("-------------");
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // gRPC 서버에 연결
    println!("Connecting to gRPC server...");
    let mut client = TicTacToeClient::connect("http://[::1]:50051").await?;
    
    // gRPC 서버로 보낼 메시지를 위한 tokio 채널 생성
    let (move_tx, move_rx) = tokio::sync::mpsc::channel(32);
    // 채널을 스트림으로 변환
    let outbound = tokio_stream::wrappers::ReceiverStream::new(move_rx);
    
    // 양방향 스트리밍 호출 (outbound 스트림을 서버로 전송)
    let response = client.play(Request::new(outbound)).await?;
    // into_parts()는 (metadata, Streaming<GameState>, extensions)를 반환합니다.
    let (_metadata, mut rx, _extensions) = response.into_parts();

    // 플레이어가 할당된 심볼("X" 또는 "O")을 저장하는 공유 변수
    let player_symbol = Arc::new(Mutex::new(None));
    // 게임 종료 여부를 저장하는 공유 변수 (false: 진행 중, true: 종료)
    let game_over = Arc::new(Mutex::new(false));
    // 게임 상태("waiting", "ongoing", "X_win", "O_win", "draw")를 저장하는 공유 변수
    let game_status = Arc::new(Mutex::new(String::new()));

    // 서버로부터 오는 게임 상태 업데이트를 수신하는 태스크
    {
        let player_symbol_clone = Arc::clone(&player_symbol);
        let game_over_clone = Arc::clone(&game_over);
        let game_status_clone = Arc::clone(&game_status);
        tokio::spawn(async move {
            while let Some(game_state) = rx.message().await.unwrap_or(None) {
                // 업데이트 받은 게임 상태를 전역 상태에 저장
                {
                    let mut status_lock = game_status_clone.lock().await;
                    *status_lock = game_state.status.clone();
                }
                println!("\n=== Game Update ===");
                if game_state.status == "waiting" {
                    // waiting 상태: 상대방 대기 메시지 출력
                    println!("Waiting for opponent to join...");
                } else if game_state.status == "ongoing" {
                    // ongoing 상태: 보드와 정보를 출력
                    print_board(&game_state.board);
                    println!("Next Player: {}", game_state.next_player);
                    println!("Your Symbol: {}", game_state.your_symbol);
                } else if game_state.status == "X_win" 
                       || game_state.status == "O_win" 
                       || game_state.status == "draw" {
                    // 게임 종료 상태: 보드와 종료 메시지 출력
                    print_board(&game_state.board);
                    println!("Game Over: {}", game_state.status);
                    let mut over_lock = game_over_clone.lock().await;
                    *over_lock = true;
                    break;
                }
                // 할당된 심볼이 없으면, 할당해줌.
                {
                    let mut sym_lock = player_symbol_clone.lock().await;
                    if sym_lock.is_none() {
                        *sym_lock = Some(game_state.your_symbol.clone());
                        println!("You are assigned: {}", game_state.your_symbol);
                    }
                }
                println!("===================\n");
            }
        });
    }

    // 터미널 입력(BufReader)을 통해 사용자 입력 처리
    let stdin = io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    println!("Enter your move (0-8), or type 'exit' to quit:");
    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("exit") {
            break;
        }

        // 게임 종료 여부를 체크
        {
            let over = game_over.lock().await;
            if *over {
                println!("Game is over. No more moves accepted.");
                break;
            }
        }
        // waiting 상태이면 입력받지 않음
        {
            let status = game_status.lock().await;
            if *status == "waiting" {
                println!("Game has not started yet. Waiting for opponent...");
                continue;
            }
        }

        // 입력값이 숫자인지 확인
        if let Ok(pos) = trimmed.parse::<usize>() {
            if pos < 9 {
                // 플레이어 심볼 확인
                let symbol_opt = {
                    let lock = player_symbol.lock().await;
                    lock.clone()
                };
                if let Some(symbol) = symbol_opt {
                    // 서버로 보낼 Move 메시지 생성
                    let mv = Move {
                        player_id: symbol.clone(),
                        position: pos as i32,
                    };
                    // 메시지를 서버로 전송
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
    }

    println!("Exiting game.");
    Ok(())
}
