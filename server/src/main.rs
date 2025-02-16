use tonic::{transport::Server, Request, Response, Status};
use tic_tac_toe::tic_tac_toe_server::{TicTacToe, TicTacToeServer};
use tic_tac_toe::{MoveRequest, MoveResponse, GameStateRequest, GameStateResponse};

pub mod tic_tac_toe {
    tonic::include_proto!("tic_tac_toe");
}

#[derive(Default)]
pub struct MyTicTacToe {}

#[tonic::async_trait]
impl TicTacToe for MyTicTacToe {
    async fn make_move(
        &self,
        request: Request<MoveRequest>,
    ) -> Result<Response<MoveResponse>, Status> {
        let request = request.into_inner();
        let reply = MoveResponse {
            message: format!("Player {} moved to ({}, {})", request.player, request.x, request.y),
        };
        Ok(Response::new(reply))
    }

    async fn get_game_state(
        &self,
        _request: Request<GameStateRequest>,
    ) -> Result<Response<GameStateResponse>, Status> {
        let reply = GameStateResponse {
            board: vec![" ".to_string(); 9],
        };
        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse()?;
    let tic_tac_toe = MyTicTacToe::default();

    println!("TicTacToeServer listening on {}", addr);

    Server::builder()
        .add_service(TicTacToeServer::new(tic_tac_toe))
        .serve(addr)
        .await?;

    Ok(())
}
