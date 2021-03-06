#![deny(unsafe_code)]
#![deny(clippy::clone_on_ref_ptr)]
#![feature(label_break_value, option_zip)]

pub mod cmd;
pub mod error;

// Reexports
pub use crate::error::Error;
pub use authc::AuthClientError;
pub use specs::{
    join::Join,
    saveload::{Marker, MarkerAllocator},
    Builder, DispatcherBuilder, Entity as EcsEntity, ReadStorage, WorldExt,
};

use byteorder::{ByteOrder, LittleEndian};
use common::{
    character::{CharacterId, CharacterItem},
    comp::{
        self,
        chat::{KillSource, KillType},
        group, ControlAction, ControlEvent, Controller, ControllerInputs, GroupManip,
        InventoryManip, InventoryUpdateEvent,
    },
    event::{EventBus, LocalEvent},
    msg::{
        validate_chat_msg, ChatMsgValidationError, ClientGeneral, ClientInGame, ClientMsg,
        ClientRegister, ClientType, DisconnectReason, InviteAnswer, Notification, PingMsg,
        PlayerInfo, PlayerListUpdate, RegisterError, ServerGeneral, ServerInfo, ServerInit,
        ServerRegisterAnswer, MAX_BYTES_CHAT_MSG,
    },
    outcome::Outcome,
    recipe::RecipeBook,
    state::State,
    sync::{Uid, UidAllocator, WorldSyncExt},
    terrain::{block::Block, neighbors, TerrainChunk, TerrainChunkSize},
    vol::RectVolSize,
};
use futures_executor::block_on;
use futures_timer::Delay;
use futures_util::{select, FutureExt};
use hashbrown::{HashMap, HashSet};
use image::DynamicImage;
use network::{Network, Participant, Pid, ProtocolAddr, Stream};
use num::traits::FloatConst;
use rayon::prelude::*;
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::{debug, error, trace, warn};
use uvth::{ThreadPool, ThreadPoolBuilder};
use vek::*;

const PING_ROLLING_AVERAGE_SECS: usize = 10;

pub enum Event {
    Chat(comp::ChatMsg),
    Disconnect,
    DisconnectionNotification(u64),
    InventoryUpdated(InventoryUpdateEvent),
    Kicked(String),
    Notification(Notification),
    SetViewDistance(u32),
    Outcome(Outcome),
}

pub struct Client {
    registered: bool,
    in_game: Option<ClientInGame>,
    thread_pool: ThreadPool,
    pub server_info: ServerInfo,
    /// Just the "base" layer for LOD; currently includes colors and nothing
    /// else. In the future we'll add more layers, like shadows, rivers, and
    /// probably foliage, cities, roads, and other structures.
    pub lod_base: Vec<u32>,
    /// The "height" layer for LOD; currently includes only land altitudes, but
    /// in the future should also water depth, and probably other
    /// information as well.
    pub lod_alt: Vec<u32>,
    /// The "shadow" layer for LOD.  Includes east and west horizon angles and
    /// an approximate max occluder height, which we use to try to
    /// approximate soft and volumetric shadows.
    pub lod_horizon: Vec<u32>,
    /// A fully rendered map image for use with the map and minimap; note that
    /// this can be constructed dynamically by combining the layers of world
    /// map data (e.g. with shadow map data or river data), but at present
    /// we opt not to do this.
    ///
    /// The second element of the tuple is the world size (as a 2D grid,
    /// in chunks), and the third element holds the minimum height for any land
    /// chunk (i.e. the sea level) in its x coordinate, and the maximum land
    /// height above this height (i.e. the max height) in its y coordinate.
    pub world_map: (Arc<DynamicImage>, Vec2<u16>, Vec2<f32>),
    pub player_list: HashMap<Uid, PlayerInfo>,
    pub character_list: CharacterList,
    pub active_character_id: Option<CharacterId>,
    recipe_book: RecipeBook,
    available_recipes: HashSet<String>,

    max_group_size: u32,
    // Client has received an invite (inviter uid, time out instant)
    group_invite: Option<(Uid, std::time::Instant, std::time::Duration)>,
    group_leader: Option<Uid>,
    // Note: potentially representable as a client only component
    group_members: HashMap<Uid, group::Role>,
    // Pending invites that this client has sent out
    pending_invites: HashSet<Uid>,

    _network: Network,
    participant: Option<Participant>,
    general_stream: Stream,
    ping_stream: Stream,
    register_stream: Stream,
    character_screen_stream: Stream,
    in_game_stream: Stream,

    client_timeout: Duration,
    last_server_ping: f64,
    last_server_pong: f64,
    last_ping_delta: f64,
    ping_deltas: VecDeque<f64>,

    tick: u64,
    state: State,
    entity: EcsEntity,

    view_distance: Option<u32>,
    // TODO: move into voxygen
    loaded_distance: f32,

    pending_chunks: HashMap<Vec2<i32>, Instant>,
}

/// Holds data related to the current players characters, as well as some
/// additional state to handle UI.
#[derive(Default)]
pub struct CharacterList {
    pub characters: Vec<CharacterItem>,
    pub loading: bool,
    pub error: Option<String>,
}

impl Client {
    /// Create a new `Client`.
    pub fn new<A: Into<SocketAddr>>(addr: A, view_distance: Option<u32>) -> Result<Self, Error> {
        let mut thread_pool = ThreadPoolBuilder::new()
            .name("veloren-worker".into())
            .build();
        // We reduce the thread count by 1 to keep rendering smooth
        thread_pool.set_num_threads((num_cpus::get() - 1).max(1));

        let (network, scheduler) = Network::new(Pid::new());
        thread_pool.execute(scheduler);

        let participant = block_on(network.connect(ProtocolAddr::Tcp(addr.into())))?;
        let stream = block_on(participant.opened())?;
        let mut ping_stream = block_on(participant.opened())?;
        let mut register_stream = block_on(participant.opened())?;
        let character_screen_stream = block_on(participant.opened())?;
        let in_game_stream = block_on(participant.opened())?;

        register_stream.send(ClientType::Game)?;
        let server_info: ServerInfo = block_on(register_stream.recv())?;

        // TODO: Display that versions don't match in Voxygen
        if server_info.git_hash != *common::util::GIT_HASH {
            warn!(
                "Server is running {}[{}], you are running {}[{}], versions might be incompatible!",
                server_info.git_hash,
                server_info.git_date,
                common::util::GIT_HASH.to_string(),
                common::util::GIT_DATE.to_string(),
            );
        }
        debug!("Auth Server: {:?}", server_info.auth_provider);

        ping_stream.send(PingMsg::Ping)?;

        // Wait for initial sync
        let (
            state,
            entity,
            lod_base,
            lod_alt,
            lod_horizon,
            world_map,
            recipe_book,
            max_group_size,
            client_timeout,
        ) = match block_on(register_stream.recv())? {
            ServerInit::GameSync {
                entity_package,
                time_of_day,
                max_group_size,
                client_timeout,
                world_map,
                recipe_book,
            } => {
                // Initialize `State`
                let mut state = State::default();
                // Client-only components
                state
                    .ecs_mut()
                    .register::<comp::Last<comp::CharacterState>>();

                let entity = state.ecs_mut().apply_entity_package(entity_package);
                *state.ecs_mut().write_resource() = time_of_day;

                let map_size_lg = common::terrain::MapSizeLg::new(world_map.dimensions_lg)
                    .map_err(|_| {
                        Error::Other(format!(
                            "Server sent bad world map dimensions: {:?}",
                            world_map.dimensions_lg,
                        ))
                    })?;
                let map_size = map_size_lg.chunks();
                let max_height = world_map.max_height;
                let sea_level = world_map.sea_level;
                let rgba = world_map.rgba;
                let alt = world_map.alt;
                let expected_size = (u32::from(map_size.x) * u32::from(map_size.y)) as usize;
                if rgba.len() != expected_size {
                    return Err(Error::Other("Server sent a bad world map image".into()));
                }
                if alt.len() != expected_size {
                    return Err(Error::Other("Server sent a bad altitude map.".into()));
                }
                let [west, east] = world_map.horizons;
                let scale_angle =
                    |a: u8| (a as f32 / 255.0 * <f32 as FloatConst>::FRAC_PI_2()).tan();
                let scale_height = |h: u8| h as f32 / 255.0 * max_height;
                let scale_height_big = |h: u32| (h >> 3) as f32 / 8191.0 * max_height;
                ping_stream.send(PingMsg::Ping)?;

                debug!("Preparing image...");
                let unzip_horizons = |(angles, heights): &(Vec<_>, Vec<_>)| {
                    (
                        angles.iter().copied().map(scale_angle).collect::<Vec<_>>(),
                        heights
                            .iter()
                            .copied()
                            .map(scale_height)
                            .collect::<Vec<_>>(),
                    )
                };
                let horizons = [unzip_horizons(&west), unzip_horizons(&east)];

                // Redraw map (with shadows this time).
                let mut world_map = vec![0u32; rgba.len()];
                let mut map_config = common::terrain::map::MapConfig::orthographic(
                    map_size_lg,
                    core::ops::RangeInclusive::new(0.0, max_height),
                );
                map_config.horizons = Some(&horizons);
                let rescale_height = |h: f32| h / max_height;
                let bounds_check = |pos: Vec2<i32>| {
                    pos.reduce_partial_min() >= 0
                        && pos.x < map_size.x as i32
                        && pos.y < map_size.y as i32
                };
                ping_stream.send(PingMsg::Ping)?;
                map_config.generate(
                    |pos| {
                        let (rgba, alt, downhill_wpos) = if bounds_check(pos) {
                            let posi = pos.y as usize * map_size.x as usize + pos.x as usize;
                            let [r, g, b, a] = rgba[posi].to_le_bytes();
                            let alti = alt[posi];
                            // Compute downhill.
                            let downhill = {
                                let mut best = -1;
                                let mut besth = alti;
                                for nposi in neighbors(map_size_lg, posi) {
                                    let nbh = alt[nposi];
                                    if nbh < besth {
                                        besth = nbh;
                                        best = nposi as isize;
                                    }
                                }
                                best
                            };
                            let downhill_wpos = if downhill < 0 {
                                None
                            } else {
                                Some(
                                    Vec2::new(
                                        (downhill as usize % map_size.x as usize) as i32,
                                        (downhill as usize / map_size.x as usize) as i32,
                                    ) * TerrainChunkSize::RECT_SIZE.map(|e| e as i32),
                                )
                            };
                            (Rgba::new(r, g, b, a), alti, downhill_wpos)
                        } else {
                            (Rgba::zero(), 0, None)
                        };
                        let wpos = pos * TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
                        let downhill_wpos = downhill_wpos
                            .unwrap_or(wpos + TerrainChunkSize::RECT_SIZE.map(|e| e as i32));
                        let alt = rescale_height(scale_height_big(alt));
                        common::terrain::map::MapSample {
                            rgb: Rgb::from(rgba),
                            alt: f64::from(alt),
                            downhill_wpos,
                            connections: None,
                        }
                    },
                    |wpos| {
                        let pos = wpos.map2(TerrainChunkSize::RECT_SIZE, |e, f| e / f as i32);
                        rescale_height(if bounds_check(pos) {
                            let posi = pos.y as usize * map_size.x as usize + pos.x as usize;
                            scale_height_big(alt[posi])
                        } else {
                            0.0
                        })
                    },
                    |pos, (r, g, b, a)| {
                        world_map[pos.y * map_size.x as usize + pos.x] =
                            u32::from_le_bytes([r, g, b, a]);
                    },
                );
                ping_stream.send(PingMsg::Ping)?;
                let make_raw = |rgba| -> Result<_, Error> {
                    let mut raw = vec![0u8; 4 * world_map.len()];
                    LittleEndian::write_u32_into(rgba, &mut raw);
                    Ok(Arc::new(
                        image::DynamicImage::ImageRgba8({
                            // Should not fail if the dimensions are correct.
                            let map =
                                image::ImageBuffer::from_raw(u32::from(map_size.x), u32::from(map_size.y), raw);
                            map.ok_or_else(|| Error::Other("Server sent a bad world map image".into()))?
                        })
                        // Flip the image, since Voxygen uses an orientation where rotation from
                        // positive x axis to positive y axis is counterclockwise around the z axis.
                        .flipv(),
                    ))
                };
                ping_stream.send(PingMsg::Ping)?;
                let lod_base = rgba;
                let lod_alt = alt;
                let world_map = make_raw(&world_map)?;
                let horizons = (west.0, west.1, east.0, east.1)
                    .into_par_iter()
                    .map(|(wa, wh, ea, eh)| u32::from_le_bytes([wa, wh, ea, eh]))
                    .collect::<Vec<_>>();
                let lod_horizon = horizons;
                let map_bounds = Vec2::new(sea_level, max_height);
                debug!("Done preparing image...");

                Ok((
                    state,
                    entity,
                    lod_base,
                    lod_alt,
                    lod_horizon,
                    (world_map, map_size, map_bounds),
                    recipe_book,
                    max_group_size,
                    client_timeout,
                ))
            },
            ServerInit::TooManyPlayers => Err(Error::TooManyPlayers),
        }?;
        ping_stream.send(PingMsg::Ping)?;

        let mut thread_pool = ThreadPoolBuilder::new()
            .name("veloren-worker".into())
            .build();
        // We reduce the thread count by 1 to keep rendering smooth
        thread_pool.set_num_threads((num_cpus::get() - 1).max(1));

        debug!("Initial sync done");

        Ok(Self {
            registered: false,
            in_game: None,
            thread_pool,
            server_info,
            world_map,
            lod_base,
            lod_alt,
            lod_horizon,
            player_list: HashMap::new(),
            character_list: CharacterList::default(),
            active_character_id: None,
            recipe_book,
            available_recipes: HashSet::default(),

            max_group_size,
            group_invite: None,
            group_leader: None,
            group_members: HashMap::new(),
            pending_invites: HashSet::new(),

            _network: network,
            participant: Some(participant),
            general_stream: stream,
            ping_stream,
            register_stream,
            character_screen_stream,
            in_game_stream,

            client_timeout,

            last_server_ping: 0.0,
            last_server_pong: 0.0,
            last_ping_delta: 0.0,
            ping_deltas: VecDeque::new(),

            tick: 0,
            state,
            entity,
            view_distance,
            loaded_distance: 0.0,

            pending_chunks: HashMap::new(),
        })
    }

    pub fn with_thread_pool(mut self, thread_pool: ThreadPool) -> Self {
        self.thread_pool = thread_pool;
        self
    }

    /// Request a state transition to `ClientState::Registered`.
    pub fn register(
        &mut self,
        username: String,
        password: String,
        mut auth_trusted: impl FnMut(&str) -> bool,
    ) -> Result<(), Error> {
        // Authentication
        let token_or_username = self.server_info.auth_provider.as_ref().map(|addr|
                // Query whether this is a trusted auth server
                if auth_trusted(&addr) {
                    Ok(authc::AuthClient::new(addr)
                        .sign_in(&username, &password)?
                        .serialize())
                } else {
                    Err(Error::AuthServerNotTrusted)
                }
        ).unwrap_or(Ok(username))?;

        self.send_msg_err(ClientRegister { token_or_username })?;

        match block_on(self.register_stream.recv::<ServerRegisterAnswer>())? {
            Err(RegisterError::AlreadyLoggedIn) => Err(Error::AlreadyLoggedIn),
            Err(RegisterError::AuthError(err)) => Err(Error::AuthErr(err)),
            Err(RegisterError::InvalidCharacter) => Err(Error::InvalidCharacter),
            Err(RegisterError::NotOnWhitelist) => Err(Error::NotOnWhitelist),
            Err(RegisterError::Banned(reason)) => Err(Error::Banned(reason)),
            Ok(()) => {
                self.registered = true;
                Ok(())
            },
        }
    }

    fn send_msg_err<S>(&mut self, msg: S) -> Result<(), network::StreamError>
    where
        S: Into<ClientMsg>,
    {
        let msg: ClientMsg = msg.into();
        #[cfg(debug_assertions)]
        {
            const C_TYPE: ClientType = ClientType::Game;
            let verified = msg.verify(C_TYPE, self.registered, self.in_game);
            assert!(
                verified,
                format!(
                    "c_type: {:?}, registered: {}, in_game: {:?}, msg: {:?}",
                    C_TYPE, self.registered, self.in_game, msg
                )
            );
        }
        match msg {
            ClientMsg::Type(msg) => self.register_stream.send(msg),
            ClientMsg::Register(msg) => self.register_stream.send(msg),
            ClientMsg::General(msg) => {
                let stream = match msg {
                    ClientGeneral::RequestCharacterList
                    | ClientGeneral::CreateCharacter { .. }
                    | ClientGeneral::DeleteCharacter(_)
                    | ClientGeneral::Character(_)
                    | ClientGeneral::Spectate => &mut self.character_screen_stream,
                    //Only in game
                    ClientGeneral::ControllerInputs(_)
                    | ClientGeneral::ControlEvent(_)
                    | ClientGeneral::ControlAction(_)
                    | ClientGeneral::SetViewDistance(_)
                    | ClientGeneral::BreakBlock(_)
                    | ClientGeneral::PlaceBlock(_, _)
                    | ClientGeneral::ExitInGame
                    | ClientGeneral::PlayerPhysics { .. }
                    | ClientGeneral::TerrainChunkRequest { .. }
                    | ClientGeneral::UnlockSkill(_)
                    | ClientGeneral::RefundSkill(_)
                    | ClientGeneral::UnlockSkillGroup(_) => &mut self.in_game_stream,
                    //Always possible
                    ClientGeneral::ChatMsg(_)
                    | ClientGeneral::Disconnect
                    | ClientGeneral::Terminate => &mut self.general_stream,
                };
                stream.send(msg)
            },
            ClientMsg::Ping(msg) => self.ping_stream.send(msg),
        }
    }

    fn send_msg<S>(&mut self, msg: S)
    where
        S: Into<ClientMsg>,
    {
        let res = self.send_msg_err(msg);
        if let Err(e) = res {
            warn!(
                ?e,
                "connection to server no longer possible, couldn't send msg"
            );
        }
    }

    /// Request a state transition to `ClientState::Character`.
    pub fn request_character(&mut self, character_id: CharacterId) {
        self.send_msg(ClientGeneral::Character(character_id));

        //Assume we are in_game unless server tells us otherwise
        self.in_game = Some(ClientInGame::Character);

        self.active_character_id = Some(character_id);
    }

    /// Load the current players character list
    pub fn load_character_list(&mut self) {
        self.character_list.loading = true;
        self.send_msg(ClientGeneral::RequestCharacterList);
    }

    /// New character creation
    pub fn create_character(&mut self, alias: String, tool: Option<String>, body: comp::Body) {
        self.character_list.loading = true;
        self.send_msg(ClientGeneral::CreateCharacter { alias, tool, body });
    }

    /// Character deletion
    pub fn delete_character(&mut self, character_id: CharacterId) {
        self.character_list.loading = true;
        self.send_msg(ClientGeneral::DeleteCharacter(character_id));
    }

    /// Send disconnect message to the server
    pub fn request_logout(&mut self) {
        debug!("Requesting logout from server");
        self.send_msg(ClientGeneral::Disconnect);
    }

    /// Request a state transition to `ClientState::Registered` from an ingame
    /// state.
    pub fn request_remove_character(&mut self) { self.send_msg(ClientGeneral::ExitInGame); }

    pub fn set_view_distance(&mut self, view_distance: u32) {
        self.view_distance = Some(view_distance.max(1).min(65));
        self.send_msg(ClientGeneral::SetViewDistance(self.view_distance.unwrap()));
    }

    pub fn use_slot(&mut self, slot: comp::slot::Slot) {
        self.send_msg(ClientGeneral::ControlEvent(ControlEvent::InventoryManip(
            InventoryManip::Use(slot),
        )));
    }

    pub fn swap_slots(&mut self, a: comp::slot::Slot, b: comp::slot::Slot) {
        self.send_msg(ClientGeneral::ControlEvent(ControlEvent::InventoryManip(
            InventoryManip::Swap(a, b),
        )));
    }

    pub fn drop_slot(&mut self, slot: comp::slot::Slot) {
        self.send_msg(ClientGeneral::ControlEvent(ControlEvent::InventoryManip(
            InventoryManip::Drop(slot),
        )));
    }

    pub fn pick_up(&mut self, entity: EcsEntity) {
        if let Some(uid) = self.state.read_component_copied(entity) {
            self.send_msg(ClientGeneral::ControlEvent(ControlEvent::InventoryManip(
                InventoryManip::Pickup(uid),
            )));
        }
    }

    pub fn recipe_book(&self) -> &RecipeBook { &self.recipe_book }

    pub fn available_recipes(&self) -> &HashSet<String> { &self.available_recipes }

    pub fn can_craft_recipe(&self, recipe: &str) -> bool {
        self.recipe_book
            .get(recipe)
            .zip(self.inventories().get(self.entity))
            .map(|(recipe, inv)| inv.contains_ingredients(&*recipe).is_ok())
            .unwrap_or(false)
    }

    pub fn craft_recipe(&mut self, recipe: &str) -> bool {
        if self.can_craft_recipe(recipe) {
            self.send_msg(ClientGeneral::ControlEvent(ControlEvent::InventoryManip(
                InventoryManip::CraftRecipe(recipe.to_string()),
            )));
            true
        } else {
            false
        }
    }

    fn update_available_recipes(&mut self) {
        self.available_recipes = self
            .recipe_book
            .iter()
            .map(|(name, _)| name.clone())
            .filter(|name| self.can_craft_recipe(name))
            .collect();
    }

    pub fn enable_lantern(&mut self) {
        self.send_msg(ClientGeneral::ControlEvent(ControlEvent::EnableLantern));
    }

    pub fn disable_lantern(&mut self) {
        self.send_msg(ClientGeneral::ControlEvent(ControlEvent::DisableLantern));
    }

    pub fn max_group_size(&self) -> u32 { self.max_group_size }

    pub fn group_invite(&self) -> Option<(Uid, std::time::Instant, std::time::Duration)> {
        self.group_invite
    }

    pub fn group_info(&self) -> Option<(String, Uid)> {
        self.group_leader.map(|l| ("Group".into(), l)) // TODO
    }

    pub fn group_members(&self) -> &HashMap<Uid, group::Role> { &self.group_members }

    pub fn pending_invites(&self) -> &HashSet<Uid> { &self.pending_invites }

    pub fn send_group_invite(&mut self, invitee: Uid) {
        self.send_msg(ClientGeneral::ControlEvent(ControlEvent::GroupManip(
            GroupManip::Invite(invitee),
        )))
    }

    pub fn accept_group_invite(&mut self) {
        // Clear invite
        self.group_invite.take();
        self.send_msg(ClientGeneral::ControlEvent(ControlEvent::GroupManip(
            GroupManip::Accept,
        )));
    }

    pub fn decline_group_invite(&mut self) {
        // Clear invite
        self.group_invite.take();
        self.send_msg(ClientGeneral::ControlEvent(ControlEvent::GroupManip(
            GroupManip::Decline,
        )));
    }

    pub fn leave_group(&mut self) {
        self.send_msg(ClientGeneral::ControlEvent(ControlEvent::GroupManip(
            GroupManip::Leave,
        )));
    }

    pub fn kick_from_group(&mut self, uid: Uid) {
        self.send_msg(ClientGeneral::ControlEvent(ControlEvent::GroupManip(
            GroupManip::Kick(uid),
        )));
    }

    pub fn assign_group_leader(&mut self, uid: Uid) {
        self.send_msg(ClientGeneral::ControlEvent(ControlEvent::GroupManip(
            GroupManip::AssignLeader(uid),
        )));
    }

    pub fn is_mounted(&self) -> bool {
        self.state
            .ecs()
            .read_storage::<comp::Mounting>()
            .get(self.entity)
            .is_some()
    }

    pub fn is_lantern_enabled(&self) -> bool {
        self.state
            .ecs()
            .read_storage::<comp::LightEmitter>()
            .get(self.entity)
            .is_some()
    }

    pub fn mount(&mut self, entity: EcsEntity) {
        if let Some(uid) = self.state.read_component_copied(entity) {
            self.send_msg(ClientGeneral::ControlEvent(ControlEvent::Mount(uid)));
        }
    }

    pub fn unmount(&mut self) { self.send_msg(ClientGeneral::ControlEvent(ControlEvent::Unmount)); }

    pub fn respawn(&mut self) {
        if self
            .state
            .ecs()
            .read_storage::<comp::Stats>()
            .get(self.entity)
            .map_or(false, |s| s.is_dead)
        {
            self.send_msg(ClientGeneral::ControlEvent(ControlEvent::Respawn));
        }
    }

    /// Checks whether a player can swap their weapon+ability `Loadout` settings
    /// and sends the `ControlAction` event that signals to do the swap.
    pub fn swap_loadout(&mut self) { self.control_action(ControlAction::SwapLoadout) }

    pub fn toggle_wield(&mut self) {
        let is_wielding = self
            .state
            .ecs()
            .read_storage::<comp::CharacterState>()
            .get(self.entity)
            .map(|cs| cs.is_wield());

        match is_wielding {
            Some(true) => self.control_action(ControlAction::Unwield),
            Some(false) => self.control_action(ControlAction::Wield),
            None => warn!("Can't toggle wield, client entity doesn't have a `CharacterState`"),
        }
    }

    pub fn toggle_sit(&mut self) {
        let is_sitting = self
            .state
            .ecs()
            .read_storage::<comp::CharacterState>()
            .get(self.entity)
            .map(|cs| matches!(cs, comp::CharacterState::Sit));

        match is_sitting {
            Some(true) => self.control_action(ControlAction::Stand),
            Some(false) => self.control_action(ControlAction::Sit),
            None => warn!("Can't toggle sit, client entity doesn't have a `CharacterState`"),
        }
    }

    pub fn toggle_dance(&mut self) {
        let is_dancing = self
            .state
            .ecs()
            .read_storage::<comp::CharacterState>()
            .get(self.entity)
            .map(|cs| matches!(cs, comp::CharacterState::Dance));

        match is_dancing {
            Some(true) => self.control_action(ControlAction::Stand),
            Some(false) => self.control_action(ControlAction::Dance),
            None => warn!("Can't toggle dance, client entity doesn't have a `CharacterState`"),
        }
    }

    pub fn toggle_sneak(&mut self) {
        let is_sneaking = self
            .state
            .ecs()
            .read_storage::<comp::CharacterState>()
            .get(self.entity)
            .map(|cs| matches!(cs, comp::CharacterState::Sneak));

        match is_sneaking {
            Some(true) => self.control_action(ControlAction::Stand),
            Some(false) => self.control_action(ControlAction::Sneak),
            None => warn!("Can't toggle sneak, client entity doesn't have a `CharacterState`"),
        }
    }

    pub fn toggle_glide(&mut self) {
        let is_gliding = self
            .state
            .ecs()
            .read_storage::<comp::CharacterState>()
            .get(self.entity)
            .map(|cs| {
                matches!(
                    cs,
                    comp::CharacterState::GlideWield | comp::CharacterState::Glide
                )
            });

        match is_gliding {
            Some(true) => self.control_action(ControlAction::Unwield),
            Some(false) => self.control_action(ControlAction::GlideWield),
            None => warn!("Can't toggle glide, client entity doesn't have a `CharacterState`"),
        }
    }

    fn control_action(&mut self, control_action: ControlAction) {
        if let Some(controller) = self
            .state
            .ecs()
            .write_storage::<Controller>()
            .get_mut(self.entity)
        {
            controller.actions.push(control_action);
        }
        self.send_msg(ClientGeneral::ControlAction(control_action));
    }

    pub fn view_distance(&self) -> Option<u32> { self.view_distance }

    pub fn loaded_distance(&self) -> f32 { self.loaded_distance }

    pub fn current_chunk(&self) -> Option<Arc<TerrainChunk>> {
        let chunk_pos = Vec2::from(
            self.state
                .read_storage::<comp::Pos>()
                .get(self.entity)
                .cloned()?
                .0,
        )
        .map2(TerrainChunkSize::RECT_SIZE, |e: f32, sz| {
            (e as u32).div_euclid(sz) as i32
        });

        self.state.terrain().get_key_arc(chunk_pos).cloned()
    }

    pub fn inventories(&self) -> ReadStorage<comp::Inventory> { self.state.read_storage() }

    pub fn loadouts(&self) -> ReadStorage<comp::Loadout> { self.state.read_storage() }

    /// Send a chat message to the server.
    pub fn send_chat(&mut self, message: String) {
        match validate_chat_msg(&message) {
            Ok(()) => self.send_msg(ClientGeneral::ChatMsg(message)),
            Err(ChatMsgValidationError::TooLong) => tracing::warn!(
                "Attempted to send a message that's too long (Over {} bytes)",
                MAX_BYTES_CHAT_MSG
            ),
        }
    }

    /// Remove all cached terrain
    pub fn clear_terrain(&mut self) {
        self.state.clear_terrain();
        self.pending_chunks.clear();
    }

    pub fn place_block(&mut self, pos: Vec3<i32>, block: Block) {
        self.send_msg(ClientGeneral::PlaceBlock(pos, block));
    }

    pub fn remove_block(&mut self, pos: Vec3<i32>) {
        self.send_msg(ClientGeneral::BreakBlock(pos));
    }

    pub fn collect_block(&mut self, pos: Vec3<i32>) {
        self.send_msg(ClientGeneral::ControlEvent(ControlEvent::InventoryManip(
            InventoryManip::Collect(pos),
        )));
    }

    /// Execute a single client tick, handle input and update the game state by
    /// the given duration.
    pub fn tick(
        &mut self,
        inputs: ControllerInputs,
        dt: Duration,
        add_foreign_systems: impl Fn(&mut DispatcherBuilder),
    ) -> Result<Vec<Event>, Error> {
        // This tick function is the centre of the Veloren universe. Most client-side
        // things are managed from here, and as such it's important that it
        // stays organised. Please consult the core developers before making
        // significant changes to this code. Here is the approximate order of
        // things. Please update it as this code changes.
        //
        // 1) Collect input from the frontend, apply input effects to the state
        //    of the game
        // 2) Handle messages from the server
        // 3) Go through any events (timer-driven or otherwise) that need handling
        //    and apply them to the state of the game
        // 4) Perform a single LocalState tick (i.e: update the world and entities
        //    in the world)
        // 5) Go through the terrain update queue and apply all changes
        //    to the terrain
        // 6) Sync information to the server
        // 7) Finish the tick, passing actions of the main thread back
        //    to the frontend

        // 1) Handle input from frontend.
        // Pass character actions from frontend input to the player's entity.
        if self.in_game.is_some() {
            if let Err(e) = self
                .state
                .ecs()
                .write_storage::<Controller>()
                .entry(self.entity)
                .map(|entry| {
                    entry
                        .or_insert_with(|| Controller {
                            inputs: inputs.clone(),
                            events: Vec::new(),
                            actions: Vec::new(),
                        })
                        .inputs = inputs.clone();
                })
            {
                let entry = self.entity;
                error!(
                    ?e,
                    ?entry,
                    "Couldn't access controller component on client entity"
                );
            }
            self.send_msg_err(ClientGeneral::ControllerInputs(inputs))?;
        }

        // 2) Build up a list of events for this frame, to be passed to the frontend.
        let mut frontend_events = Vec::new();

        // Prepare for new events
        {
            let ecs = self.state.ecs();
            for (entity, _) in (&ecs.entities(), &ecs.read_storage::<comp::Body>()).join() {
                let mut last_character_states =
                    ecs.write_storage::<comp::Last<comp::CharacterState>>();
                if let Some(client_character_state) =
                    ecs.read_storage::<comp::CharacterState>().get(entity)
                {
                    if last_character_states
                        .get(entity)
                        .map(|l| !client_character_state.same_variant(&l.0))
                        .unwrap_or(true)
                    {
                        let _ = last_character_states
                            .insert(entity, comp::Last(client_character_state.clone()));
                    }
                }
            }
        }

        // Handle new messages from the server.
        frontend_events.append(&mut self.handle_new_messages()?);

        // 3) Update client local data
        // Check if the group invite has timed out and remove if so
        if self
            .group_invite
            .map_or(false, |(_, timeout, dur)| timeout.elapsed() > dur)
        {
            self.group_invite = None;
        }

        // 4) Tick the client's LocalState
        self.state.tick(dt, add_foreign_systems, true);

        // 5) Terrain
        let pos = self
            .state
            .read_storage::<comp::Pos>()
            .get(self.entity)
            .cloned();
        if let (Some(pos), Some(view_distance)) = (pos, self.view_distance) {
            let chunk_pos = self.state.terrain().pos_key(pos.0.map(|e| e as i32));

            // Remove chunks that are too far from the player.
            let mut chunks_to_remove = Vec::new();
            self.state.terrain().iter().for_each(|(key, _)| {
                // Subtract 2 from the offset before computing squared magnitude
                // 1 for the chunks needed bordering other chunks for meshing
                // 1 as a buffer so that if the player moves back in that direction the chunks
                //   don't need to be reloaded
                if (chunk_pos - key)
                    .map(|e: i32| (e.abs() as u32).saturating_sub(2))
                    .magnitude_squared()
                    > view_distance.pow(2)
                {
                    chunks_to_remove.push(key);
                }
            });
            for key in chunks_to_remove {
                self.state.remove_chunk(key);
            }

            // Request chunks from the server.
            self.loaded_distance = ((view_distance * TerrainChunkSize::RECT_SIZE.x) as f32).powi(2);
            // +1 so we can find a chunk that's outside the vd for better fog
            for dist in 0..view_distance as i32 + 1 {
                // Only iterate through chunks that need to be loaded for circular vd
                // The (dist - 2) explained:
                // -0.5 because a chunk is visible if its corner is within the view distance
                // -0.5 for being able to move to the corner of the current chunk
                // -1 because chunks are not meshed if they don't have all their neighbors
                //     (notice also that view_distance is decreased by 1)
                //     (this subtraction on vd is omitted elsewhere in order to provide
                //     a buffer layer of loaded chunks)
                let top = if 2 * (dist - 2).max(0).pow(2) > (view_distance - 1).pow(2) as i32 {
                    ((view_distance - 1).pow(2) as f32 - (dist - 2).pow(2) as f32)
                        .sqrt()
                        .round() as i32
                        + 1
                } else {
                    dist
                };

                let mut skip_mode = false;
                for i in -top..top + 1 {
                    let keys = [
                        chunk_pos + Vec2::new(dist, i),
                        chunk_pos + Vec2::new(i, dist),
                        chunk_pos + Vec2::new(-dist, i),
                        chunk_pos + Vec2::new(i, -dist),
                    ];

                    for key in keys.iter() {
                        if self.state.terrain().get_key(*key).is_none() {
                            if !skip_mode && !self.pending_chunks.contains_key(key) {
                                if self.pending_chunks.len() < 4 {
                                    self.send_msg_err(ClientGeneral::TerrainChunkRequest {
                                        key: *key,
                                    })?;
                                    self.pending_chunks.insert(*key, Instant::now());
                                } else {
                                    skip_mode = true;
                                }
                            }

                            let dist_to_player =
                                (self.state.terrain().key_pos(*key).map(|x| x as f32)
                                    + TerrainChunkSize::RECT_SIZE.map(|x| x as f32) / 2.0)
                                    .distance_squared(pos.0.into());

                            if dist_to_player < self.loaded_distance {
                                self.loaded_distance = dist_to_player;
                            }
                        }
                    }
                }
            }
            self.loaded_distance = self.loaded_distance.sqrt()
                - ((TerrainChunkSize::RECT_SIZE.x as f32 / 2.0).powi(2)
                    + (TerrainChunkSize::RECT_SIZE.y as f32 / 2.0).powi(2))
                .sqrt();

            // If chunks are taking too long, assume they're no longer pending.
            let now = Instant::now();
            self.pending_chunks
                .retain(|_, created| now.duration_since(*created) < Duration::from_secs(3));
        }

        // Send a ping to the server once every second
        if self.state.get_time() - self.last_server_ping > 1. {
            self.send_msg_err(PingMsg::Ping)?;
            self.last_server_ping = self.state.get_time();
        }

        // 6) Update the server about the player's physics attributes.
        if self.in_game.is_some() {
            if let (Some(pos), Some(vel), Some(ori)) = (
                self.state.read_storage().get(self.entity).cloned(),
                self.state.read_storage().get(self.entity).cloned(),
                self.state.read_storage().get(self.entity).cloned(),
            ) {
                self.in_game_stream
                    .send(ClientGeneral::PlayerPhysics { pos, vel, ori })?;
            }
        }

        /*
        // Output debug metrics
        if log_enabled!(Level::Info) && self.tick % 600 == 0 {
            let metrics = self
                .state
                .terrain()
                .iter()
                .fold(ChonkMetrics::default(), |a, (_, c)| a + c.get_metrics());
            info!("{:?}", metrics);
        }
        */

        // 7) Finish the tick, pass control back to the frontend.
        self.tick += 1;
        Ok(frontend_events)
    }

    /// Clean up the client after a tick.
    pub fn cleanup(&mut self) {
        // Cleanup the local state
        self.state.cleanup();
    }

    fn handle_server_msg(
        &mut self,
        frontend_events: &mut Vec<Event>,
        msg: ServerGeneral,
    ) -> Result<(), Error> {
        match msg {
            ServerGeneral::Disconnect(reason) => match reason {
                DisconnectReason::Shutdown => return Err(Error::ServerShutdown),
                DisconnectReason::Requested => {
                    debug!("finally sending ClientMsg::Terminate");
                    frontend_events.push(Event::Disconnect);
                    self.send_msg_err(ClientGeneral::Terminate)?;
                },
                DisconnectReason::Kicked(reason) => {
                    debug!("sending ClientMsg::Terminate because we got kicked");
                    frontend_events.push(Event::Kicked(reason));
                    self.send_msg_err(ClientGeneral::Terminate)?;
                },
            },
            ServerGeneral::PlayerListUpdate(PlayerListUpdate::Init(list)) => {
                self.player_list = list
            },
            ServerGeneral::PlayerListUpdate(PlayerListUpdate::Add(uid, player_info)) => {
                if let Some(old_player_info) = self.player_list.insert(uid, player_info.clone()) {
                    warn!(
                        "Received msg to insert {} with uid {} into the player list but there was \
                         already an entry for {} with the same uid that was overwritten!",
                        player_info.player_alias, uid, old_player_info.player_alias
                    );
                }
            },
            ServerGeneral::PlayerListUpdate(PlayerListUpdate::Admin(uid, admin)) => {
                if let Some(player_info) = self.player_list.get_mut(&uid) {
                    player_info.is_admin = admin;
                } else {
                    warn!(
                        "Received msg to update admin status of uid {}, but they were not in the \
                         list.",
                        uid
                    );
                }
            },
            ServerGeneral::PlayerListUpdate(PlayerListUpdate::SelectedCharacter(
                uid,
                char_info,
            )) => {
                if let Some(player_info) = self.player_list.get_mut(&uid) {
                    player_info.character = Some(char_info);
                } else {
                    warn!(
                        "Received msg to update character info for uid {}, but they were not in \
                         the list.",
                        uid
                    );
                }
            },
            ServerGeneral::PlayerListUpdate(PlayerListUpdate::LevelChange(uid, next_level)) => {
                if let Some(player_info) = self.player_list.get_mut(&uid) {
                    player_info.character = match &player_info.character {
                        Some(character) => Some(common::msg::CharacterInfo {
                            name: character.name.to_string(),
                            level: next_level,
                        }),
                        None => {
                            warn!(
                                "Received msg to update character level info to {} for uid {}, \
                                 but this player's character is None.",
                                next_level, uid
                            );

                            None
                        },
                    };
                }
            },
            ServerGeneral::PlayerListUpdate(PlayerListUpdate::Remove(uid)) => {
                // Instead of removing players, mark them as offline because we need to
                // remember the names of disconnected players in chat.
                //
                // TODO the server should re-use uids of players that log out and log back
                // in.

                if let Some(player_info) = self.player_list.get_mut(&uid) {
                    if player_info.is_online {
                        player_info.is_online = false;
                    } else {
                        warn!(
                            "Received msg to remove uid {} from the player list by they were \
                             already marked offline",
                            uid
                        );
                    }
                } else {
                    warn!(
                        "Received msg to remove uid {} from the player list by they weren't in \
                         the list!",
                        uid
                    );
                }
            },
            ServerGeneral::PlayerListUpdate(PlayerListUpdate::Alias(uid, new_name)) => {
                if let Some(player_info) = self.player_list.get_mut(&uid) {
                    player_info.player_alias = new_name;
                } else {
                    warn!(
                        "Received msg to alias player with uid {} to {} but this uid is not in \
                         the player list",
                        uid, new_name
                    );
                }
            },
            ServerGeneral::ChatMsg(m) => frontend_events.push(Event::Chat(m)),
            ServerGeneral::SetPlayerEntity(uid) => {
                if let Some(entity) = self.state.ecs().entity_from_uid(uid.0) {
                    self.entity = entity;
                } else {
                    return Err(Error::Other("Failed to find entity from uid.".to_owned()));
                }
            },
            ServerGeneral::TimeOfDay(time_of_day) => {
                *self.state.ecs_mut().write_resource() = time_of_day;
            },
            ServerGeneral::EntitySync(entity_sync_package) => {
                self.state
                    .ecs_mut()
                    .apply_entity_sync_package(entity_sync_package);
            },
            ServerGeneral::CompSync(comp_sync_package) => {
                self.state
                    .ecs_mut()
                    .apply_comp_sync_package(comp_sync_package);
            },
            ServerGeneral::CreateEntity(entity_package) => {
                self.state.ecs_mut().apply_entity_package(entity_package);
            },
            ServerGeneral::DeleteEntity(entity) => {
                if self.uid() != Some(entity) {
                    self.state
                        .ecs_mut()
                        .delete_entity_and_clear_from_uid_allocator(entity.0);
                }
            },
            ServerGeneral::Notification(n) => {
                frontend_events.push(Event::Notification(n));
            },
            _ => unreachable!("Not a general msg"),
        }
        Ok(())
    }

    fn handle_server_in_game_msg(
        &mut self,
        frontend_events: &mut Vec<Event>,
        msg: ServerGeneral,
    ) -> Result<(), Error> {
        match msg {
            ServerGeneral::GroupUpdate(change_notification) => {
                use comp::group::ChangeNotification::*;
                // Note: we use a hashmap since this would not work with entities outside
                // the view distance
                match change_notification {
                    Added(uid, role) => {
                        // Check if this is a newly formed group by looking for absence of
                        // other non pet group members
                        if !matches!(role, group::Role::Pet)
                            && !self
                                .group_members
                                .values()
                                .any(|r| !matches!(r, group::Role::Pet))
                        {
                            frontend_events
                                .push(Event::Chat(comp::ChatType::Meta.chat_msg(
                                    "Type /g or /group to chat with your group members",
                                )));
                        }
                        if let Some(player_info) = self.player_list.get(&uid) {
                            frontend_events.push(Event::Chat(
                                comp::ChatType::GroupMeta("Group".into()).chat_msg(format!(
                                    "[{}] joined group",
                                    self.personalize_alias(uid, player_info.player_alias.clone())
                                )),
                            ));
                        }
                        if self.group_members.insert(uid, role) == Some(role) {
                            warn!(
                                "Received msg to add uid {} to the group members but they were \
                                 already there",
                                uid
                            );
                        }
                    },
                    Removed(uid) => {
                        if let Some(player_info) = self.player_list.get(&uid) {
                            frontend_events.push(Event::Chat(
                                comp::ChatType::GroupMeta("Group".into()).chat_msg(format!(
                                    "[{}] left group",
                                    self.personalize_alias(uid, player_info.player_alias.clone())
                                )),
                            ));
                        }
                        if self.group_members.remove(&uid).is_none() {
                            warn!(
                                "Received msg to remove uid {} from group members but by they \
                                 weren't in there!",
                                uid
                            );
                        }
                    },
                    NewLeader(leader) => {
                        self.group_leader = Some(leader);
                    },
                    NewGroup { leader, members } => {
                        self.group_leader = Some(leader);
                        self.group_members = members.into_iter().collect();
                        // Currently add/remove messages treat client as an implicit member
                        // of the group whereas this message explicitly includes them so to
                        // be consistent for now we will remove the client from the
                        // received hashset
                        if let Some(uid) = self.uid() {
                            self.group_members.remove(&uid);
                        }
                    },
                    NoGroup => {
                        self.group_leader = None;
                        self.group_members = HashMap::new();
                    },
                }
            },
            ServerGeneral::GroupInvite { inviter, timeout } => {
                self.group_invite = Some((inviter, std::time::Instant::now(), timeout));
            },
            ServerGeneral::InvitePending(uid) => {
                if !self.pending_invites.insert(uid) {
                    warn!("Received message about pending invite that was already pending");
                }
            },
            ServerGeneral::InviteComplete { target, answer } => {
                if !self.pending_invites.remove(&target) {
                    warn!(
                        "Received completed invite message for invite that was not in the list of \
                         pending invites"
                    )
                }
                // TODO: expose this as a new event variant instead of going
                // through the chat
                let msg = match answer {
                    // TODO: say who accepted/declined/timed out the invite
                    InviteAnswer::Accepted => "Invite accepted",
                    InviteAnswer::Declined => "Invite declined",
                    InviteAnswer::TimedOut => "Invite timed out",
                };
                frontend_events.push(Event::Chat(comp::ChatType::Meta.chat_msg(msg)));
            },
            // Cleanup for when the client goes back to the `in_game = None`
            ServerGeneral::ExitInGameSuccess => {
                self.in_game = None;
                self.clean_state();
            },
            ServerGeneral::InventoryUpdate(mut inventory, event) => {
                match event {
                    InventoryUpdateEvent::CollectFailed => {},
                    _ => {
                        inventory.recount_items();
                        // Push the updated inventory component to the client
                        self.state.write_component(self.entity, inventory);
                    },
                }

                self.update_available_recipes();

                frontend_events.push(Event::InventoryUpdated(event));
            },
            ServerGeneral::TerrainChunkUpdate { key, chunk } => {
                if let Ok(chunk) = chunk {
                    self.state.insert_chunk(key, *chunk);
                }
                self.pending_chunks.remove(&key);
            },
            ServerGeneral::TerrainBlockUpdates(mut blocks) => {
                blocks.drain().for_each(|(pos, block)| {
                    self.state.set_block(pos, block);
                });
            },
            ServerGeneral::SetViewDistance(vd) => {
                self.view_distance = Some(vd);
                frontend_events.push(Event::SetViewDistance(vd));
            },
            ServerGeneral::Outcomes(outcomes) => {
                frontend_events.extend(outcomes.into_iter().map(Event::Outcome))
            },
            ServerGeneral::Knockback(impulse) => {
                self.state
                    .ecs()
                    .read_resource::<EventBus<LocalEvent>>()
                    .emit_now(LocalEvent::ApplyImpulse {
                        entity: self.entity,
                        impulse,
                    });
            },
            _ => unreachable!("Not a in_game message"),
        }
        Ok(())
    }

    fn handle_server_character_screen_msg(&mut self, msg: ServerGeneral) -> Result<(), Error> {
        match msg {
            ServerGeneral::CharacterListUpdate(character_list) => {
                self.character_list.characters = character_list;
                self.character_list.loading = false;
            },
            ServerGeneral::CharacterActionError(error) => {
                warn!("CharacterActionError: {:?}.", error);
                self.character_list.error = Some(error);
            },
            ServerGeneral::CharacterDataLoadError(error) => {
                trace!("Handling join error by server");
                self.in_game = None;
                self.clean_state();
                self.character_list.error = Some(error);
            },
            ServerGeneral::CharacterSuccess => {
                debug!("client is now in ingame state on server");
                if let Some(vd) = self.view_distance {
                    self.set_view_distance(vd);
                }
            },
            _ => unreachable!("Not a character_screen msg"),
        }
        Ok(())
    }

    fn handle_ping_msg(&mut self, msg: PingMsg) -> Result<(), Error> {
        match msg {
            PingMsg::Ping => {
                self.send_msg_err(PingMsg::Pong)?;
            },
            PingMsg::Pong => {
                self.last_server_pong = self.state.get_time();
                self.last_ping_delta = self.state.get_time() - self.last_server_ping;

                // Maintain the correct number of deltas for calculating the rolling average
                // ping. The client sends a ping to the server every second so we should be
                // receiving a pong reply roughly every second.
                while self.ping_deltas.len() > PING_ROLLING_AVERAGE_SECS - 1 {
                    self.ping_deltas.pop_front();
                }
                self.ping_deltas.push_back(self.last_ping_delta);
            },
        }
        Ok(())
    }

    async fn handle_messages(
        &mut self,
        frontend_events: &mut Vec<Event>,
        cnt: &mut u64,
    ) -> Result<(), Error> {
        loop {
            let (m1, m2, m3, m4) = select!(
                msg = self.general_stream.recv().fuse() => (Some(msg), None, None, None),
                msg = self.ping_stream.recv().fuse() => (None, Some(msg), None, None),
                msg = self.character_screen_stream.recv().fuse() => (None, None, Some(msg), None),
                msg = self.in_game_stream.recv().fuse() => (None, None, None, Some(msg)),
            );
            *cnt += 1;
            if let Some(msg) = m1 {
                self.handle_server_msg(frontend_events, msg?)?;
            }
            if let Some(msg) = m2 {
                self.handle_ping_msg(msg?)?;
            }
            if let Some(msg) = m3 {
                self.handle_server_character_screen_msg(msg?)?;
            }
            if let Some(msg) = m4 {
                self.handle_server_in_game_msg(frontend_events, msg?)?;
            }
        }
    }

    /// Handle new server messages.
    fn handle_new_messages(&mut self) -> Result<Vec<Event>, Error> {
        let mut frontend_events = Vec::new();

        // Check that we have an valid connection.
        // Use the last ping time as a 1s rate limiter, we only notify the user once per
        // second
        if self.state.get_time() - self.last_server_ping > 1. {
            let duration_since_last_pong = self.state.get_time() - self.last_server_pong;

            // Dispatch a notification to the HUD warning they will be kicked in {n} seconds
            const KICK_WARNING_AFTER_REL_TO_TIMEOUT_FRACTION: f64 = 0.75;
            if duration_since_last_pong
                >= (self.client_timeout.as_secs() as f64
                    * KICK_WARNING_AFTER_REL_TO_TIMEOUT_FRACTION)
                && self.state.get_time() - duration_since_last_pong > 0.
            {
                frontend_events.push(Event::DisconnectionNotification(
                    (self.state.get_time() - duration_since_last_pong).round() as u64,
                ));
            }
        }

        let mut handles_msg = 0;

        block_on(async {
            //TIMEOUT 0.01 ms for msg handling
            select!(
                _ = Delay::new(std::time::Duration::from_micros(10)).fuse() => Ok(()),
                err = self.handle_messages(&mut frontend_events, &mut handles_msg).fuse() => err,
            )
        })?;

        if handles_msg == 0
            && self.state.get_time() - self.last_server_pong > self.client_timeout.as_secs() as f64
        {
            return Err(Error::ServerTimeout);
        }

        Ok(frontend_events)
    }

    pub fn entity(&self) -> EcsEntity { self.entity }

    pub fn uid(&self) -> Option<Uid> { self.state.read_component_copied(self.entity) }

    pub fn in_game(&self) -> Option<ClientInGame> { self.in_game }

    pub fn registered(&self) -> bool { self.registered }

    pub fn get_tick(&self) -> u64 { self.tick }

    pub fn get_ping_ms(&self) -> f64 { self.last_ping_delta * 1000.0 }

    pub fn get_ping_ms_rolling_avg(&self) -> f64 {
        let mut total_weight = 0.;
        let pings = self.ping_deltas.len() as f64;
        (self
            .ping_deltas
            .iter()
            .enumerate()
            .fold(0., |acc, (i, ping)| {
                let weight = i as f64 + 1. / pings;
                total_weight += weight;
                acc + (weight * ping)
            })
            / total_weight)
            * 1000.0
    }

    /// Get a reference to the client's worker thread pool. This pool should be
    /// used for any computationally expensive operations that run outside
    /// of the main thread (i.e., threads that block on I/O operations are
    /// exempt).
    pub fn thread_pool(&self) -> &ThreadPool { &self.thread_pool }

    /// Get a reference to the client's game state.
    pub fn state(&self) -> &State { &self.state }

    /// Get a mutable reference to the client's game state.
    pub fn state_mut(&mut self) -> &mut State { &mut self.state }

    /// Get a vector of all the players on the server
    pub fn get_players(&mut self) -> Vec<comp::Player> {
        // TODO: Don't clone players.
        self.state
            .ecs()
            .read_storage::<comp::Player>()
            .join()
            .cloned()
            .collect()
    }

    /// Return true if this client is an admin on the server
    pub fn is_admin(&self) -> bool {
        let client_uid = self
            .state
            .read_component_copied::<Uid>(self.entity)
            .expect("Client doesn't have a Uid!!!");

        self.player_list
            .get(&client_uid)
            .map_or(false, |info| info.is_admin)
    }

    /// Clean client ECS state
    fn clean_state(&mut self) {
        let client_uid = self
            .uid()
            .map(|u| u.into())
            .expect("Client doesn't have a Uid!!!");

        // Clear ecs of all entities
        self.state.ecs_mut().delete_all();
        self.state.ecs_mut().maintain();
        self.state.ecs_mut().insert(UidAllocator::default());

        // Recreate client entity with Uid
        let entity_builder = self.state.ecs_mut().create_entity();
        let uid = entity_builder
            .world
            .write_resource::<UidAllocator>()
            .allocate(entity_builder.entity, Some(client_uid));

        self.entity = entity_builder.with(uid).build();
    }

    /// Change player alias to "You" if client belongs to matching player
    fn personalize_alias(&self, uid: Uid, alias: String) -> String {
        let client_uid = self.uid().expect("Client doesn't have a Uid!!!");
        if client_uid == uid {
            "You".to_string() // TODO: Localize
        } else {
            alias
        }
    }

    /// Format a message for the client (voxygen chat box or chat-cli)
    pub fn format_message(&self, msg: &comp::ChatMsg, character_name: bool) -> String {
        let comp::ChatMsg {
            chat_type, message, ..
        } = &msg;
        let alias_of_uid = |uid| {
            self.player_list
                .get(uid)
                .map_or("<?>".to_string(), |player_info| {
                    if player_info.is_admin {
                        format!(
                            "ADMIN - {}",
                            self.personalize_alias(*uid, player_info.player_alias.clone())
                        )
                    } else {
                        self.personalize_alias(*uid, player_info.player_alias.clone())
                    }
                })
        };
        let name_of_uid = |uid| {
            let ecs = self.state.ecs();
            (
                &ecs.read_storage::<comp::Stats>(),
                &ecs.read_storage::<Uid>(),
            )
                .join()
                .find(|(_, u)| u == &uid)
                .map(|(c, _)| c.name.clone())
        };
        let message_format = |uid, message, group| {
            let alias = alias_of_uid(uid);
            let name = if character_name {
                name_of_uid(uid)
            } else {
                None
            };
            match (group, name) {
                (Some(group), None) => format!("({}) [{}]: {}", group, alias, message),
                (None, None) => format!("[{}]: {}", alias, message),
                (Some(group), Some(name)) => {
                    format!("({}) [{}] {}: {}", group, alias, name, message)
                },
                (None, Some(name)) => format!("[{}] {}: {}", alias, name, message),
            }
        };
        match chat_type {
            // For ChatType::{Online, Offline, Kill} these message strings are localized
            // in voxygen/src/hud/chat.rs before being formatted here.
            // Kill messages are generated in server/src/events/entity_manipulation.rs
            // fn handle_destroy
            comp::ChatType::Online(uid) => {
                // Default message formats if no localized message string is set by hud
                // Needed for cli clients that don't set localization info
                if message == "" {
                    format!("[{}] came online", alias_of_uid(uid))
                } else {
                    message.replace("{name}", &alias_of_uid(uid))
                }
            },
            comp::ChatType::Offline(uid) => {
                // Default message formats if no localized message string is set by hud
                // Needed for cli clients that don't set localization info
                if message == "" {
                    format!("[{}] went offline", alias_of_uid(uid))
                } else {
                    message.replace("{name}", &alias_of_uid(uid))
                }
            },
            comp::ChatType::CommandError => message.to_string(),
            comp::ChatType::CommandInfo => message.to_string(),
            comp::ChatType::Loot => message.to_string(),
            comp::ChatType::FactionMeta(_) => message.to_string(),
            comp::ChatType::GroupMeta(_) => message.to_string(),
            comp::ChatType::Kill(kill_source, victim) => {
                // Default message formats if no localized message string is set by hud
                // Needed for cli clients that don't set localization info
                if message == "" {
                    match kill_source {
                        KillSource::Player(attacker_uid, KillType::Melee) => format!(
                            "[{}] killed [{}]",
                            alias_of_uid(attacker_uid),
                            alias_of_uid(victim)
                        ),
                        KillSource::Player(attacker_uid, KillType::Projectile) => format!(
                            "[{}] shot [{}]",
                            alias_of_uid(attacker_uid),
                            alias_of_uid(victim)
                        ),
                        KillSource::Player(attacker_uid, KillType::Explosion) => format!(
                            "[{}] blew up [{}]",
                            alias_of_uid(attacker_uid),
                            alias_of_uid(victim)
                        ),
                        KillSource::Player(attacker_uid, KillType::Energy) => format!(
                            "[{}] used magic to kill [{}]",
                            alias_of_uid(attacker_uid),
                            alias_of_uid(victim)
                        ),
                        KillSource::NonPlayer(attacker_name, KillType::Melee) => {
                            format!("{} killed [{}]", attacker_name, alias_of_uid(victim))
                        },
                        KillSource::NonPlayer(attacker_name, KillType::Projectile) => {
                            format!("{} shot [{}]", attacker_name, alias_of_uid(victim))
                        },
                        KillSource::NonPlayer(attacker_name, KillType::Explosion) => {
                            format!("{} blew up [{}]", attacker_name, alias_of_uid(victim))
                        },
                        KillSource::NonPlayer(attacker_name, KillType::Energy) => format!(
                            "{} used magic to kill [{}]",
                            attacker_name,
                            alias_of_uid(victim)
                        ),
                        KillSource::Environment(environment) => {
                            format!("[{}] died in {}", alias_of_uid(victim), environment)
                        },
                        KillSource::FallDamage => {
                            format!("[{}] died from fall damage", alias_of_uid(victim))
                        },
                        KillSource::Suicide => {
                            format!("[{}] died from self-inflicted wounds", alias_of_uid(victim))
                        },
                        KillSource::Other => format!("[{}] died", alias_of_uid(victim)),
                    }
                } else {
                    match kill_source {
                        KillSource::Player(attacker_uid, KillType::Melee) => message
                            .replace("{attacker}", &alias_of_uid(attacker_uid))
                            .replace("{victim}", &alias_of_uid(victim)),
                        KillSource::Player(attacker_uid, KillType::Projectile) => message
                            .replace("{attacker}", &alias_of_uid(attacker_uid))
                            .replace("{victim}", &alias_of_uid(victim)),
                        KillSource::Player(attacker_uid, KillType::Explosion) => message
                            .replace("{attacker}", &alias_of_uid(attacker_uid))
                            .replace("{victim}", &alias_of_uid(victim)),
                        KillSource::Player(attacker_uid, KillType::Energy) => message
                            .replace("{attacker}", &alias_of_uid(attacker_uid))
                            .replace("{victim}", &alias_of_uid(victim)),
                        KillSource::NonPlayer(attacker_name, KillType::Melee) => message
                            .replace("{attacker}", attacker_name)
                            .replace("{victim}", &alias_of_uid(victim)),
                        KillSource::NonPlayer(attacker_name, KillType::Projectile) => message
                            .replace("{attacker}", attacker_name)
                            .replace("{victim}", &alias_of_uid(victim)),
                        KillSource::NonPlayer(attacker_name, KillType::Explosion) => message
                            .replace("{attacker}", attacker_name)
                            .replace("{victim}", &alias_of_uid(victim)),
                        KillSource::NonPlayer(attacker_name, KillType::Energy) => message
                            .replace("{attacker}", attacker_name)
                            .replace("{victim}", &alias_of_uid(victim)),
                        KillSource::Environment(environment) => message
                            .replace("{name}", &alias_of_uid(victim))
                            .replace("{environment}", environment),
                        KillSource::FallDamage => message.replace("{name}", &alias_of_uid(victim)),
                        KillSource::Suicide => message.replace("{name}", &alias_of_uid(victim)),
                        KillSource::Other => message.replace("{name}", &alias_of_uid(victim)),
                    }
                }
            },
            comp::ChatType::Tell(from, to) => {
                let from_alias = alias_of_uid(from);
                let to_alias = alias_of_uid(to);
                if Some(*from) == self.uid() {
                    format!("To [{}]: {}", to_alias, message)
                } else {
                    format!("From [{}]: {}", from_alias, message)
                }
            },
            comp::ChatType::Say(uid) => message_format(uid, message, None),
            comp::ChatType::Group(uid, s) => message_format(uid, message, Some(s)),
            comp::ChatType::Faction(uid, s) => message_format(uid, message, Some(s)),
            comp::ChatType::Region(uid) => message_format(uid, message, None),
            comp::ChatType::World(uid) => message_format(uid, message, None),
            // NPCs can't talk. Should be filtered by hud/mod.rs for voxygen and should be filtered
            // by server (due to not having a Pos) for chat-cli
            comp::ChatType::Npc(_uid, _r) => "".to_string(),
            comp::ChatType::Meta => message.to_string(),
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        trace!("Dropping client");
        if self.registered {
            if let Err(e) = self.send_msg_err(ClientGeneral::Disconnect) {
                warn!(
                    ?e,
                    "Error during drop of client, couldn't send disconnect package, is the \
                     connection already closed?",
                );
            }
        } else {
            trace!("no disconnect msg necessary as client wasn't registered")
        }
        if let Err(e) = block_on(self.participant.take().unwrap().disconnect()) {
            warn!(?e, "error when disconnecting, couldn't send all data");
        }
    }
}
