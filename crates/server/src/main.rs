use crossbeam::channel::{Receiver, Sender};
use game::{ClientPacket, Region, RegionPacket};

/// Represents a region of the world. After going though ingress, the client
/// connects to this instance directly to download game data and send / receive
/// events.
///
/// Server instances communicate with one another on behalf of the player when
/// moving between regions.
///
/// ## Initial Setup
///
/// - Load world from SQLite database or generate from file.
/// - Enter main loop
///
/// ## Game Loop
/// - Order incoming client packets by game tick.
/// - Check if client packets are for the current game tick and execute them
///   if so.
/// - Execute game tick.
/// - If the loop didn't take a full TICK_TIME, wait until full TICK_TIME has passed.
pub struct ServerInstance {
    game: Region,
    client_sender: Sender<ClientPacket>,
    region_sender: Sender<RegionPacket>,
    receiver: Receiver<RegionPacket>,
}

enum GamePacket {
    RegionPacket(RegionPacket),
    ClientPacket(ClientPacket),
}

/// ## Network Loop
/// - Process incoming client packets and send them to the game loop
/// - Receive server packets from game loop and send them out to connected
///   clients. Maintain a buffer of events for each client and drop the client
///   if the buffer grows too large.
/// - Recieve region packets from game loop and send them to their assorted
///   regions.
pub struct Network {}

fn main() {
    println!("Hello, Server!");
}
