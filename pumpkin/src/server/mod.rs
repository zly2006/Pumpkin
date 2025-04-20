use crate::block::registry::BlockRegistry;
use crate::command::commands::default_dispatcher;
use crate::command::commands::defaultgamemode::DefaultGamemode;
use crate::data::player_server_data::ServerPlayerData;
use crate::entity::EntityId;
use crate::item::registry::ItemRegistry;
use crate::net::EncryptionError;
use crate::plugin::player::player_login::PlayerLoginEvent;
use crate::plugin::server::server_broadcast::ServerBroadcastEvent;
use crate::world::custom_bossbar::CustomBossbars;
use crate::{
    command::dispatcher::CommandDispatcher, entity::player::Player, net::Client, world::World,
};
use bytes::Bytes;
use connection_cache::{CachedBranding, CachedStatus};
use key_store::KeyStore;
use pumpkin_config::{BASIC_CONFIG, advanced_config};
use pumpkin_data::block::Block;
use pumpkin_inventory::drag_handler::DragHandler;
use pumpkin_inventory::{Container, OpenContainer};
use pumpkin_macros::send_cancellable;
use pumpkin_protocol::client::login::CEncryptionRequest;
use pumpkin_protocol::{ClientPacket, client::config::CPluginMessage};
use pumpkin_registry::{DimensionType, Registry};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector2::Vector2;
use pumpkin_util::text::TextComponent;
use pumpkin_world::dimension::Dimension;
use rand::prelude::SliceRandom;
use rsa::RsaPublicKey;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::AtomicU32;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;

mod connection_cache;
mod key_store;
pub mod seasonal_events;
pub mod ticker;

pub const CURRENT_MC_VERSION: &str = "1.21.5";

/// Represents a Minecraft server instance.
pub struct Server {
    /// Handles cryptographic keys for secure communication.
    key_store: KeyStore,
    /// Manages server status information.
    listing: Mutex<CachedStatus>,
    /// Saves server branding information.
    branding: CachedBranding,
    /// Saves and dispatches commands to appropriate handlers.
    pub command_dispatcher: RwLock<CommandDispatcher>,
    /// Block behaviour.
    pub block_registry: Arc<BlockRegistry>,
    /// Item behaviour.
    pub item_registry: Arc<ItemRegistry>,
    /// Manages multiple worlds within the server.
    pub worlds: RwLock<Vec<Arc<World>>>,
    // All the dimensions that exist on the server.
    pub dimensions: Vec<DimensionType>,
    /// Caches game registries for efficient access.
    pub cached_registry: Vec<Registry>,
    /// Tracks open containers used for item interactions.
    // TODO: should have per player open_containers
    pub open_containers: RwLock<HashMap<u64, OpenContainer>>,
    pub drag_handler: DragHandler,
    /// Assigns unique IDs to containers.
    container_id: AtomicU32,
    /// Manages authentication with an authentication server, if enabled.
    pub auth_client: Option<reqwest::Client>,
    /// Mojang's public keys, used for chat session signing
    /// Pulled from Mojang API on startup
    pub mojang_public_keys: Mutex<Vec<RsaPublicKey>>,
    /// The server's custom bossbars
    pub bossbars: Mutex<CustomBossbars>,
    /// The default gamemode when a player joins the server (reset every restart)
    pub defaultgamemode: Mutex<DefaultGamemode>,
    /// Manages player data storage
    pub player_data_storage: ServerPlayerData,
    tasks: TaskTracker,
    /// nanoseconds per tick
    pub nanos: Arc<RwLock<Vec<u64>>>,
}

impl Server {
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new(nanos: Arc<RwLock<Vec<u64>>>) -> Self {
        let auth_client = BASIC_CONFIG.online_mode.then(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_millis(u64::from(
                    advanced_config().networking.authentication.connect_timeout,
                )))
                .read_timeout(Duration::from_millis(u64::from(
                    advanced_config().networking.authentication.read_timeout,
                )))
                .build()
                .expect("Failed to to make reqwest client")
        });

        // First register the default commands. After that, plugins can put in their own.
        let command_dispatcher = RwLock::new(default_dispatcher());
        let world_path = BASIC_CONFIG.get_world_path();

        let block_registry = super::block::default_registry();

        let world = World::load(
            Dimension::Overworld.into_level(world_path.clone()),
            DimensionType::Overworld,
            block_registry.clone(),
        );

        let world_name = world_path.to_str().unwrap();

        Self {
            cached_registry: Registry::get_synced(),
            open_containers: RwLock::new(HashMap::new()),
            drag_handler: DragHandler::new(),
            container_id: 0.into(),
            worlds: RwLock::new(vec![Arc::new(world)]),
            dimensions: vec![
                DimensionType::Overworld,
                DimensionType::OverworldCaves,
                DimensionType::TheNether,
                DimensionType::TheEnd,
            ],
            command_dispatcher,
            block_registry,
            item_registry: super::item::items::default_registry(),
            auth_client,
            key_store: KeyStore::new(),
            listing: Mutex::new(CachedStatus::new()),
            branding: CachedBranding::new(),
            bossbars: Mutex::new(CustomBossbars::new()),
            defaultgamemode: Mutex::new(DefaultGamemode {
                gamemode: BASIC_CONFIG.default_gamemode,
            }),
            player_data_storage: ServerPlayerData::new(
                format!("{world_name}/playerdata"),
                Duration::from_secs(advanced_config().player_data.save_player_cron_interval),
            ),
            tasks: TaskTracker::new(),
            mojang_public_keys: Mutex::new(Vec::new()),
            nanos,
        }
    }

    const SPAWN_CHUNK_RADIUS: i32 = 1;

    #[must_use]
    pub fn spawn_chunks() -> Box<[Vector2<i32>]> {
        (-Self::SPAWN_CHUNK_RADIUS..=Self::SPAWN_CHUNK_RADIUS)
            .flat_map(|x| {
                (-Self::SPAWN_CHUNK_RADIUS..=Self::SPAWN_CHUNK_RADIUS)
                    .map(move |z| Vector2::new(x, z))
            })
            .collect()
    }

    /// Spawns a task associated with this server. All tasks spawned with this method are awaited
    /// when the server stops. This means tasks should complete in a reasonable (no looping) amount of time.
    pub fn spawn_task<F>(&self, task: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.tasks.spawn(task)
    }

    /// Adds a new player to the server.
    ///
    /// This function takes an `Arc<Client>` representing the connected client and performs the following actions:
    ///
    /// 1. Generates a new entity ID for the player.
    /// 2. Determines the player's gamemode (defaulting to Survival if not specified in configuration).
    /// 3. **(TODO: Select default from config)** Selects the world for the player (currently uses the first world).
    /// 4. Creates a new `Player` instance using the provided information.
    /// 5. Adds the player to the chosen world.
    /// 6. **(TODO: Config if we want increase online)** Optionally updates server listing information based on the player's configuration.
    ///
    /// # Arguments
    ///
    /// * `client`: An `Arc<Client>` representing the connected client.
    ///
    /// # Returns
    ///
    /// A tuple containing:
    ///
    /// - `Arc<Player>`: A reference to the newly created player object.
    /// - `Arc<World>`: A reference to the world the player was added to.
    ///
    /// # Note
    ///
    /// You still have to spawn the `Player` in a `World` to let them join and make them visible.
    pub async fn add_player(&self, client: Client) -> Option<(Arc<Player>, Arc<World>)> {
        let gamemode = self.defaultgamemode.lock().await.gamemode;
        // Basically the default world
        // TODO: select default from config
        let world = &self.worlds.read().await[0];

        let mut player = Player::new(client, world.clone(), gamemode).await;

        // Load player data
        if let Err(e) = self
            .player_data_storage
            .handle_player_join(&mut player)
            .await
        {
            log::error!("Unexpected error loading player data: {e}");
        }

        // Wrap in Arc after data is loaded
        let player = Arc::new(player);

        send_cancellable! {{
            PlayerLoginEvent::new(player.clone(), TextComponent::text("You have been kicked from the server"));
            'after: {
                world
                    .add_player(player.gameprofile.id, player.clone())
                    .await;
                // TODO: Config if we want increase online
                if let Some(config) = player.client.config.lock().await.as_ref() {
                    // TODO: Config so we can also just ignore this hehe
                    if config.server_listing {
                        self.listing.lock().await.add_player();
                    }
                }

                Some((player, world.clone()))
            }

            'cancelled: {
                player.kick(event.kick_message).await;
                None
            }
        }}
    }

    pub async fn remove_player(&self) {
        // TODO: Config if we want decrease online
        self.listing.lock().await.remove_player();
    }

    pub async fn shutdown(&self) {
        self.tasks.close();
        log::debug!("Awaiting tasks for server");
        self.tasks.wait().await;
        log::debug!("Done awaiting tasks for server");

        log::info!("Starting worlds");
        for world in self.worlds.read().await.iter() {
            world.shutdown().await;
        }
        log::info!("Completed worlds");
    }

    pub async fn try_get_container(
        &self,
        player_id: EntityId,
        container_id: u64,
    ) -> Option<Arc<Mutex<Box<dyn Container>>>> {
        let open_containers = self.open_containers.read().await;
        open_containers
            .get(&container_id)?
            .try_open(player_id)
            .cloned()
    }

    /// Returns the first id with a matching location and block type. If this is used with unique
    /// blocks, the output will return a random result.
    pub async fn get_container_id(&self, location: BlockPos, block: Block) -> Option<u32> {
        let open_containers = self.open_containers.read().await;
        // TODO: do better than brute force
        for (id, container) in open_containers.iter() {
            if container.is_location(location) {
                if let Some(container_block) = container.get_block() {
                    if container_block.id == block.id {
                        log::debug!("Found container id: {id}");
                        return Some(*id as u32);
                    }
                }
            }
        }

        drop(open_containers);

        None
    }

    pub async fn get_all_container_ids(
        &self,
        location: BlockPos,
        block: Block,
    ) -> Option<Vec<u32>> {
        let open_containers = self.open_containers.read().await;
        let mut matching_container_ids: Vec<u32> = vec![];
        // TODO: do better than brute force
        for (id, container) in open_containers.iter() {
            if container.is_location(location) {
                if let Some(container_block) = container.get_block() {
                    if container_block.id == block.id {
                        log::debug!("Found matching container id: {id}");
                        matching_container_ids.push(*id as u32);
                    }
                }
            }
        }

        drop(open_containers);

        Some(matching_container_ids)
    }

    /// Broadcasts a packet to all players in all worlds.
    ///
    /// This function sends the specified packet to every connected player in every world managed by the server.
    ///
    /// # Arguments
    ///
    /// * `packet`: A reference to the packet to be broadcast. The packet must implement the `ClientPacket` trait.
    pub async fn broadcast_packet_all<P>(&self, packet: &P)
    where
        P: ClientPacket,
    {
        let mut packet_buf = Vec::new();
        if let Err(err) = packet.write(&mut packet_buf) {
            log::error!("Failed to serialize packet {}: {}", P::PACKET_ID, err);
            return;
        }
        let packet_data: Bytes = packet_buf.into();

        for world in self.worlds.read().await.iter() {
            let current_players = world.players.read().await;
            for player in current_players.values() {
                player.client.enqueue_packet_data(packet_data.clone()).await;
            }
        }
    }

    pub async fn broadcast_message(
        &self,
        message: &TextComponent,
        sender_name: &TextComponent,
        chat_type: u8,
        target_name: Option<&TextComponent>,
    ) {
        send_cancellable! {{
            ServerBroadcastEvent::new(message.clone(), sender_name.clone());

            'after: {
                for world in self.worlds.read().await.iter() {
                    world
                        .broadcast_message(&event.message, &event.sender, chat_type, target_name)
                        .await;
                }
            }
        }}
    }

    /// Searches for a player by their username across all worlds.
    ///
    /// This function iterates through each world managed by the server and attempts to find a player with the specified username.
    /// If a player is found in any world, it returns an `Arc<Player>` reference to that player. Otherwise, it returns `None`.
    ///
    /// # Arguments
    ///
    /// * `name`: The username of the player to search for.
    ///
    /// # Returns
    ///
    /// An `Option<Arc<Player>>` containing the player if found, or `None` if not found.
    pub async fn get_player_by_name(&self, name: &str) -> Option<Arc<Player>> {
        for world in self.worlds.read().await.iter() {
            if let Some(player) = world.get_player_by_name(name).await {
                return Some(player);
            }
        }
        None
    }

    pub async fn get_players_by_ip(&self, ip: IpAddr) -> Vec<Arc<Player>> {
        let mut players = Vec::<Arc<Player>>::new();

        for world in self.worlds.read().await.iter() {
            for player in world.players.read().await.values() {
                if player.client.address.lock().await.ip() == ip {
                    players.push(player.clone());
                }
            }
        }

        players
    }

    /// Returns all players from all worlds.
    pub async fn get_all_players(&self) -> Vec<Arc<Player>> {
        let mut players = Vec::<Arc<Player>>::new();

        for world in self.worlds.read().await.iter() {
            for player in world.players.read().await.values() {
                players.push(player.clone());
            }
        }

        players
    }

    /// Returns a random player from any of the worlds, or `None` if all worlds are empty.
    pub async fn get_random_player(&self) -> Option<Arc<Player>> {
        let players = self.get_all_players().await;

        players.choose(&mut rand::thread_rng()).map(Arc::<_>::clone)
    }

    /// Searches for a player by their UUID across all worlds.
    ///
    /// This function iterates through each world managed by the server and attempts to find a player with the specified UUID.
    /// If a player is found in any world, it returns an `Arc<Player>` reference to that player. Otherwise, it returns `None`.
    ///
    /// # Arguments
    ///
    /// * `id`: The UUID of the player to search for.
    ///
    /// # Returns
    ///
    /// An `Option<Arc<Player>>` containing the player if found, or `None` if not found.
    pub async fn get_player_by_uuid(&self, id: uuid::Uuid) -> Option<Arc<Player>> {
        for world in self.worlds.read().await.iter() {
            if let Some(player) = world.get_player_by_uuid(id).await {
                return Some(player);
            }
        }
        None
    }

    /// Counts the total number of players across all worlds.
    ///
    /// This function iterates through each world and sums up the number of players currently connected to that world.
    ///
    /// # Returns
    ///
    /// The total number of players connected to the server.
    pub async fn get_player_count(&self) -> usize {
        let mut count = 0;
        for world in self.worlds.read().await.iter() {
            count += world.players.read().await.len();
        }
        count
    }

    /// Similar to [`Server::get_player_count`] >= n, but may be more efficient since it stops its iteration through all worlds as soon as n players were found.
    pub async fn has_n_players(&self, n: usize) -> bool {
        let mut count = 0;
        for world in self.worlds.read().await.iter() {
            count += world.players.read().await.len();
            if count >= n {
                return true;
            }
        }
        false
    }

    /// Generates a new container id.
    pub fn new_container_id(&self) -> u32 {
        self.container_id.fetch_add(1, Ordering::SeqCst)
    }

    pub fn get_branding(&self) -> CPluginMessage<'_> {
        self.branding.get_branding()
    }

    pub fn get_status(&self) -> &Mutex<CachedStatus> {
        &self.listing
    }

    pub fn encryption_request<'a>(
        &'a self,
        verification_token: &'a [u8; 4],
        should_authenticate: bool,
    ) -> CEncryptionRequest<'a> {
        self.key_store
            .encryption_request("", verification_token, should_authenticate)
    }

    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        self.key_store.decrypt(data)
    }

    pub fn digest_secret(&self, secret: &[u8]) -> String {
        self.key_store.get_digest(secret)
    }

    async fn tick(&self) {
        for world in self.worlds.read().await.iter() {
            world.tick(self).await;
        }

        if let Err(e) = self.player_data_storage.tick(self).await {
            log::error!("Error ticking player data: {e}");
        }
    }
}
