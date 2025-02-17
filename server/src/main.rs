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

/// 서버에서 클라이언트로 전송할 스트림 타입
type ResponseStream = Pin<Box<dyn Stream<Item = Result<GameState, Status>> + Send>>;

////////////////////////////
// 1. 데이터 구조체 및 헬퍼 //
////////////////////////////

/// 각 플레이어의 연결 정보를 저장합니다.
#[derive(Clone)]
struct PlayerConnection {
    symbol: String,              // "X" 또는 "O"
    tx: mpsc::Sender<GameState>, // 업데이트 전송 채널
}

/// 게임의 전체 상태를 저장하는 구조체입니다.
#[derive(Default)]
struct SharedGame {
    board: Vec<String>,       // 9칸 보드 (각 칸: "", "X", "O")
    next_player: String,      // 다음 차례 ("X" 또는 "O")
    status: String,           // "waiting", "ongoing", "X_win", "O_win", "draw"
    player_x: Option<PlayerConnection>,
    player_o: Option<PlayerConnection>,
}

impl SharedGame {
    /// 초기 게임 상태 생성
    fn new() -> Self {
        SharedGame {
            board: vec!["".into(); 9],
            next_player: "X".into(),
            status: "waiting".into(),
            player_x: None,
            player_o: None,
        }
    }

    /// 보드가 가득 찼는지 검사
    fn is_full(&self) -> bool {
        self.board.iter().all(|cell| !cell.is_empty())
    }

    /// 승리 조건 검사
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
        for &(a, b_idx, c) in &lines {
            if !b[a].is_empty() && b[a] == b[b_idx] && b[b_idx] == b[c] {
                return Some(b[a].clone());
            }
        }
        None
    }

    /// 현재 게임 상태를 기반으로 기본 업데이트 메시지를 생성
    fn create_update(&self) -> GameState {
        GameState {
            board: self.board.clone(),
            next_player: self.next_player.clone(),
            status: self.status.clone(),
            your_symbol: String::new(), // 각 클라이언트마다 개별 설정 예정
            error_message: "".into(),    // 기본적으로 오류 메시지는 비어 있음
        }
    }

    /// 모든 연결된 플레이어에게 업데이트 메시지 전송
    async fn broadcast_update(&self) {
        let mut update = self.create_update();
        if let Some(ref player_x) = self.player_x {
            update.your_symbol = player_x.symbol.clone();
            let _ = player_x.tx.send(update.clone()).await;
        }
        if let Some(ref player_o) = self.player_o {
            update.your_symbol = player_o.symbol.clone();
            let _ = player_o.tx.send(update.clone()).await;
        }
    }

    /// 지정된 플레이어에게 오류 메시지를 전송합니다.
    async fn send_error(&self, symbol: &str, error_msg: &str) {
        let mut update = self.create_update();
        update.status = "error".into(); // 오류 상태로 설정
        update.error_message = error_msg.to_string();
        update.your_symbol = symbol.to_string();
        if symbol == "X" {
            if let Some(ref player_x) = self.player_x {
                let _ = player_x.tx.send(update).await;
            }
        } else if symbol == "O" {
            if let Some(ref player_o) = self.player_o {
                let _ = player_o.tx.send(update).await;
            }
        }
    }
}

////////////////////////////
// 2. gRPC 서비스 구현     //
////////////////////////////

/// gRPC 서비스 구조체 (게임 상태 공유)
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
        let (tx, rx) = mpsc::channel(32);
        let mut assigned_symbol = String::new();

        // 플레이어 할당 및 초기 상태 전송
        {
            let mut game = self.game.lock().await;
            if game.player_x.is_none() {
                assigned_symbol = "X".to_string();
                game.player_x = Some(PlayerConnection {
                    symbol: assigned_symbol.clone(),
                    tx: tx.clone(),
                });
                println!("플레이어 X 할당");
                let init_state = GameState {
                    board: game.board.clone(),
                    next_player: game.next_player.clone(),
                    status: game.status.clone(),
                    your_symbol: assigned_symbol.clone(),
                    error_message: "".into(),
                };
                let _ = tx.clone().try_send(init_state);
            } else if game.player_o.is_none() {
                assigned_symbol = "O".to_string();
                game.player_o = Some(PlayerConnection {
                    symbol: assigned_symbol.clone(),
                    tx: tx.clone(),
                });
                game.status = "ongoing".to_string();
                println!("플레이어 O 할당, 게임 시작 (ongoing)");
                game.broadcast_update().await;
            } else {
                return Err(Status::resource_exhausted("이미 두 명의 플레이어가 접속되어 있습니다."));
            }
        }

        // 클라이언트가 보내는 이동(Move) 메시지 처리
        let game_clone = self.game.clone();
        let symbol_clone = assigned_symbol.clone();
        let mut inbound = request.into_inner();

        tokio::spawn(async move {
            while let Some(result) = inbound.message().await.transpose() {
                match result {
                    Ok(mv) => {
                        println!("플레이어 {}가 {}번 칸에 두려 함", symbol_clone, mv.position);
                        let mut game = game_clone.lock().await;
                        // 게임이 진행 중인지 검사
                        if game.status != "ongoing" {
                            println!("게임이 진행 중이 아님");
                            game.send_error(&symbol_clone, "Game is not ongoing.").await;
                            continue;
                        }
                        // 차례 확인
                        if game.next_player != symbol_clone {
                            println!("현재 차례 아님: {}", symbol_clone);
                            game.send_error(&symbol_clone, "It's not your turn.").await;
                            continue;
                        }
                        let pos = mv.position as usize;
                        // 위치 유효성 검사
                        if pos >= 9 {
                            println!("잘못된 위치: {}", pos);
                            game.send_error(&symbol_clone, "Invalid position.").await;
                            continue;
                        }
                        if !game.board[pos].is_empty() {
                            println!("칸 {}이 이미 채워짐", pos);
                            game.send_error(&symbol_clone, "Cell already occupied.").await;
                            continue;
                        }
                        // 이동 적용
                        game.board[pos] = symbol_clone.clone();
                        // 승리 검사 및 상태 업데이트
                        if let Some(winner) = game.check_winner() {
                            game.status = format!("{}_win", winner);
                        } else if game.is_full() {
                            game.status = "draw".to_string();
                        } else {
                            game.next_player = if symbol_clone == "X" { "O".into() } else { "X".into() };
                        }
                        // 모든 플레이어에게 업데이트 전송
                        game.broadcast_update().await;
                    }
                    Err(e) => {
                        println!("메시지 수신 에러: {:?}", e);
                        break;
                    }
                }
            }
            println!("플레이어 {} 접속 종료", symbol_clone);
            let mut game = game_clone.lock().await;
            
            game.player_x = None;
            game.player_o = None;
            game.board = vec!["".into(); 9];
            game.next_player = "X".into();
            game.status = "waiting".to_string();
        });

        let output_stream = Box::pin(ReceiverStream::new(rx).map(|state| Ok(state)));
        Ok(Response::new(output_stream))
    }
}

////////////////////////////
// 3. 서버 실행           //
////////////////////////////

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
