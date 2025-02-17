use tonic::{transport::Server, Request, Response, Status};
use tokio::sync::{Mutex, mpsc};
use futures::Stream;
use std::{pin::Pin, sync::Arc};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};

pub mod tictactoe {
    tonic::include_proto!("tictactoe");
}

use tictactoe::tic_tac_toe_server::{TicTacToe, TicTacToeServer};
use tictactoe::{GameState, Move};

/// 스트리밍 응답 타입 alias
type ResponseStream = Pin<Box<dyn Stream<Item = Result<GameState, Status>> + Send>>;

/// 각 클라이언트 연결을 나타내는 구조체 (플레이어 심볼과 해당 채널)
struct PlayerConnection {
    symbol: String, // "X" 또는 "O"
    tx: mpsc::Sender<GameState>,
}

/// 게임 전체 상태를 공유하는 구조체  
/// - board: 9칸 보드 ("" 또는 "X", "O")  
/// - next_player: 다음 차례 ("X" 또는 "O")  
/// - status: "waiting", "ongoing", "X_win", "O_win", "draw"  
/// - player_x/player_o: 각각의 플레이어 연결 (없으면 None)
#[derive(Default)]
struct SharedGame {
    board: Vec<String>,
    next_player: String,
    status: String,
    player_x: Option<PlayerConnection>,
    player_o: Option<PlayerConnection>,
}

impl SharedGame {
    fn new() -> Self {
        SharedGame {
            board: vec!["".into(); 9],
            next_player: "X".into(),
            status: "waiting".into(), // 플레이어가 2명 모일 때까지 대기
            player_x: None,
            player_o: None,
        }
    }

    /// 보드가 꽉 찼는지 검사
    fn is_full(&self) -> bool {
        self.board.iter().all(|cell| !cell.is_empty())
    }

    /// 승리 조건 검사 (가로, 세로, 대각선)
    fn check_winner(&self) -> Option<String> {
        let b = &self.board;
        let lines = [
            (0, 1, 2),
            (3, 4, 5),
            (6, 7, 8),
            (0, 3, 6),
            (1, 4, 7),
            (2, 5, 8),
            (0, 4, 8),
            (2, 4, 6),
        ];
        for &(a, b_idx, c) in lines.iter() {
            if !b[a].is_empty() && b[a] == b[b_idx] && b[b_idx] == b[c] {
                return Some(b[a].clone());
            }
        }
        None
    }
}

/// gRPC 서비스 구현 구조체 (SharedGame를 공유)
#[derive(Clone)]
struct TicTacToeService {
    game: Arc<Mutex<SharedGame>>,
}

#[tonic::async_trait]
impl TicTacToe for TicTacToeService {
    type PlayStream = ResponseStream;

    async fn play(
        &self,
        request: Request<tonic::Streaming<Move>>,
    ) -> Result<Response<Self::PlayStream>, Status> {
        println!("새 클라이언트 접속: {:?}", request.remote_addr());
        
        // 클라이언트로 게임 상태 업데이트를 보내기 위한 채널 생성
        let (tx, rx) = mpsc::channel(32);
        let mut assigned_symbol = String::new();

        {
            // 게임 상태를 잠금하여 플레이어 할당
            let mut game = self.game.lock().await;
            if game.player_x.is_none() {
                assigned_symbol = "X".to_string();
                game.player_x = Some(PlayerConnection {
                    symbol: "X".to_string(),
                    tx: tx.clone(),
                });
                println!("플레이어 X 할당");
                
                // 첫 번째 플레이어에게 초기 상태 전송
                let initial_state = GameState {
                    board: game.board.clone(),
                    next_player: game.next_player.clone(),
                    status: game.status.clone(),
                    your_symbol: assigned_symbol.clone(),
                };
                if let Err(e) = tx.clone().try_send(initial_state) {
                    println!("초기 상태 전송 에러: {:?}", e);
                }
            } else if game.player_o.is_none() {
                assigned_symbol = "O".to_string();
                game.player_o = Some(PlayerConnection {
                    symbol: "O".to_string(),
                    tx: tx.clone(),
                });
                // 두 번째 플레이어가 들어오면 게임 시작
                game.status = "ongoing".to_string();
                println!("플레이어 O 할당, 게임 시작 (ongoing)");

                // 업데이트 메시지 준비 (모든 플레이어에게 전송)
                let update = GameState {
                    board: game.board.clone(),
                    next_player: game.next_player.clone(),
                    status: game.status.clone(),
                    your_symbol: "".to_string(), // 각 클라이언트에 맞게 수정될 예정
                };
                // 첫 번째 플레이어 업데이트
                if let Some(ref player_x) = game.player_x {
                    let mut update_x = update.clone();
                    update_x.your_symbol = player_x.symbol.clone();
                    let _ = player_x.tx.send(update_x).await;
                }
                // 두 번째 플레이어 업데이트
                if let Some(ref player_o) = game.player_o {
                    let mut update_o = update.clone();
                    update_o.your_symbol = player_o.symbol.clone();
                    let _ = player_o.tx.send(update_o).await;
                }
            } else {
                // 이미 두 플레이어가 접속한 경우 에러 반환
                return Err(Status::resource_exhausted(
                    "이미 두 명의 플레이어가 접속되어 있습니다.",
                ));
            }
        }

        // 클라이언트의 move 스트림을 처리하기 위해 게임 상태 클론과 할당 심볼 저장
        let game_clone = self.game.clone();
        let symbol_clone = assigned_symbol.clone();
        let mut inbound = request.into_inner();

        // 클라이언트로부터 들어오는 메시지(이동)를 처리하는 태스크 스폰
        tokio::spawn(async move {
            while let Some(result) = inbound.message().await.transpose() {
                match result {
                    Ok(mv) => {
                        println!(
                            "플레이어 {}가 {}번 칸에 두려 함",
                            symbol_clone, mv.position
                        );
                        let mut game = game_clone.lock().await;
                        // 게임이 진행 중인지 확인
                        if game.status != "ongoing" {
                            println!("게임 상태가 진행중이 아님");
                            continue;
                        }
                        // 해당 플레이어의 차례인지 확인
                        if game.next_player != symbol_clone {
                            println!("현재 차례가 아님: {}", symbol_clone);
                            continue;
                        }
                        // 올바른 위치(0~8)인지 및 빈 칸인지 확인
                        let pos = mv.position as usize;
                        if pos >= 9 {
                            println!("잘못된 위치: {}", pos);
                            continue;
                        }
                        if !game.board[pos].is_empty() {
                            println!("칸 {}이 이미 채워짐", pos);
                            continue;
                        }
                        // 이동 적용
                        game.board[pos] = symbol_clone.clone();

                        // 승리 검사
                        if let Some(winner) = game.check_winner() {
                            game.status = format!("{}_win", winner);
                        } else if game.is_full() {
                            game.status = "draw".to_string();
                        } else {
                            // 차례 변경
                            game.next_player = if symbol_clone == "X" {
                                "O".into()
                            } else {
                                "X".into()
                            };
                        }

                        // 업데이트 메시지 준비 (각 클라이언트에 맞게 your_symbol을 채워 전송)
                        let update = GameState {
                            board: game.board.clone(),
                            next_player: game.next_player.clone(),
                            status: game.status.clone(),
                            your_symbol: "".to_string(),
                        };

                        if let Some(ref player_x) = game.player_x {
                            let mut update_x = update.clone();
                            update_x.your_symbol = player_x.symbol.clone();
                            let _ = player_x.tx.send(update_x).await;
                        }
                        if let Some(ref player_o) = game.player_o {
                            let mut update_o = update.clone();
                            update_o.your_symbol = player_o.symbol.clone();
                            let _ = player_o.tx.send(update_o).await;
                        }
                    }
                    Err(e) => {
                        println!("메시지 수신 에러: {:?}", e);
                        break;
                    }
                }
            }
            println!("플레이어 {} 접속 종료", symbol_clone);
            // 접속 종료 시 해당 플레이어 제거 및 게임 초기화
            let mut game = game_clone.lock().await;
            if game
                .player_x
                .as_ref()
                .map(|p| p.symbol.clone())
                == Some(symbol_clone.clone())
            {
                game.player_x = None;
            }
            if game
                .player_o
                .as_ref()
                .map(|p| p.symbol.clone())
                == Some(symbol_clone.clone())
            {
                game.player_o = None;
            }
            game.board = vec!["".into(); 9];
            game.next_player = "X".into();
            game.status = "waiting".into();
        });

        // 클라이언트에 대해 ReceiverStream을 생성하여 gRPC 응답 스트림으로 반환
        let output_stream = Box::pin(
            ReceiverStream::new(rx)
                .map(|state| Ok(state)) // 각 GameState를 Ok(GameState)로 매핑
        );
        Ok(Response::new(output_stream))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse()?;
    println!("TicTacToeServer가 {}에서 실행 중입니다", addr);

    let game = Arc::new(Mutex::new(SharedGame::new()));
    let service = TicTacToeService { game };

    Server::builder()
        .add_service(TicTacToeServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
