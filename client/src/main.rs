use tonic::transport::Channel;
use tic_tac_toe::tic_tac_toe_client::TicTacToeClient;
use tic_tac_toe::{MoveRequest, GameStateRequest};

pub mod tic_tac_toe {
    tonic::include_proto!("tic_tac_toe");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = TicTacToeClient::connect("http://[::1]:50051").await?;

    let request = tonic::Request::new(MoveRequest {
        player: "X".to_string(),
        x: 1,
        y: 1,
    });

    let response = client.make_move(request).await?;
    println!("Move Response: {:?}", response.into_inner().message);

    let request = tonic::Request::new(GameStateRequest {});
    let response = client.get_game_state(request).await?;
    println!("Game State: {:?}", response.into_inner().board);

    Ok(())
}