pub mod commands;
pub mod plugins;

#[macro_use]
extern crate tracing;

use anyhow::{Context, Result};
use azalea::{
    blocks::Block,
    core::direction::Direction,
    ecs::query::With,
    entity::{metadata::Player, EyeHeight, Pose, Position},
    inventory::{InventoryComponent, ItemSlot, SetSelectedHotbarSlotEvent},
    packet_handling::game::SendPacketEvent,
    pathfinder::{goals::BlockPosGoal, Pathfinder},
    prelude::*,
    protocol::packets::game::{
        serverbound_interact_packet::InteractionHand,
        serverbound_player_action_packet::ServerboundPlayerActionPacket,
        serverbound_set_carried_item_packet::ServerboundSetCarriedItemPacket,
        serverbound_use_item_on_packet::{BlockHit, ServerboundUseItemOnPacket},
        serverbound_use_item_packet::ServerboundUseItemPacket,
        ClientboundGamePacket, ServerboundGamePacket,
    },
    registry::{EntityKind, Item},
    swarm::{Swarm, SwarmEvent},
    world::MinecraftEntityId,
    GameProfileComponent, JoinOpts, Vec3,
};
use clap::Parser;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    fmt::Debug,
    io::IsTerminal,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tracing_subscriber::prelude::*;

/// A simple stasis bot, using azalea!
#[derive(Parser)]
#[clap(author, version)]
struct Opts {
    /// What server ((and port) to connect to
    server_address: String,

    /// Player names who are considered more trustworthy for certain commands
    #[clap(short, long)]
    admin: Vec<String>,

    /// Use ViaProxy to translate the protocol to the given minecraft version.
    /// Conflicts with --openauthmod
    #[clap(short, long)]
    via: Option<String>,

    /// Automatically log out, once getting to this HP or lower (or a totem pops)
    #[clap(short = 'H', long)]
    autolog_hp: Option<f32>,

    /// Workaround for crashes: Forbid the bot from sending any messages to players.
    #[clap(short = 'q', long)]
    quiet: bool,

    /// Make the stasis bot, not do any stasis-related duties. So you can abuse him easier as an afk bot.
    #[clap(short = 'S', long)]
    no_stasis: bool,

    /// Specify a logfile to log everything into as well
    #[clap(short = 'l', long)]
    log_file: Option<PathBuf>,

    /// Disable color. Can fix some issues of still persistent escape codes in log files.
    #[clap(short = 'C', long)]
    no_color: bool,

    /// Allow chat messages to be signed.
    #[clap(short = 's', long)]
    sign_chat: bool,

    /// Use an offline account with specified user name.
    #[clap(long)]
    offline_username: Option<String>,

    /// Use OpenAuthMod. Enables using this account across proxies like ViaProxy.
    /// Conflicts with --via
    #[clap(short = 'A', long)]
    openauthmod: bool,

    /// Forbid pathfinding to mine blocks to reach its goal
    #[clap(short = 'M', long)]
    no_mining: bool,

    /// Enable looking at the closest player which is no more than N blocks away.
    #[clap(short = 'L', long)]
    look_at_players: Option<u32>,

    /// Enable a command, that allows admins to get the position of the bot. Might be dangerous!
    #[clap(long)]
    enable_pos_command: bool,

    /// Enables Automatic Eating food items in hotbar, when appropriate
    #[clap(long)]
    auto_eat: bool,

    /// File, used to store authentication information in. Ignored if --offline-username is used.
    #[clap(long, default_value = "login-secrets.json")]
    auth_file: PathBuf,
}

pub const FOOD_ITEMS: &[Item] = &[
    Item::Apple,
    Item::GoldenApple,
    Item::EnchantedGoldenApple,
    Item::Carrot,
    Item::GoldenCarrot,
    Item::MelonSlice,
    Item::SweetBerries,
    Item::GlowBerries,
    Item::Potato,
    Item::BakedPotato,
    Item::Beetroot,
    Item::DriedKelp,
    Item::Beef,
    Item::CookedBeef,
    Item::Porkchop,
    Item::CookedPorkchop,
    Item::Mutton,
    Item::CookedMutton,
    Item::Chicken,
    Item::CookedChicken,
    Item::Rabbit,
    Item::CookedRabbit,
    Item::Cod,
    Item::CookedCod,
    Item::Salmon,
    Item::CookedSalmon,
    Item::TropicalFish,
    Item::Bread,
    Item::Cookie,
    Item::PumpkinPie,
    Item::MushroomStew,
    Item::BeetrootSoup,
    Item::RabbitStew,
];

static OPTS: Lazy<Opts> = Lazy::new(|| Opts::parse());
static INPUTLINE_QUEUE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
struct BlockPos {
    x: i32,
    y: i32,
    z: i32,
}

impl From<azalea::BlockPos> for BlockPos {
    fn from(value: azalea::BlockPos) -> Self {
        Self {
            x: value.x,
            y: value.y,
            z: value.z,
        }
    }
}

impl From<BlockPos> for azalea::BlockPos {
    fn from(value: BlockPos) -> Self {
        Self {
            x: value.x,
            y: value.y,
            z: value.z,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse cli args, handle --help, etc.
    let _ = *OPTS;

    // Setup logging
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    let reg = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_ansi(!OPTS.no_color),
        );
    if let Some(logfile_path) = &OPTS.log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(logfile_path)
            .context("Open logfile for appending")?;
        reg.with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file),
        )
        .init();
    } else {
        reg.init();
    }

    if OPTS.offline_username.is_none() {
        info!(
            "File used to store Authentication information: {:?}",
            OPTS.auth_file
        );
    }

    if OPTS.openauthmod && OPTS.via.is_some() {
        error!("-v/--via and -A/--openauthmod cannot be used together! Choose only one.");
        std::process::exit(1);
    }

    if OPTS.no_color {
        info!("Will not use colored chat messages and disabled ansi formatting in console.");
    }

    if OPTS.quiet {
        info!("Will not automatically send any chat commands (workaround for getting kicked because of broken ChatCommand packet).");
    }

    if let Some(autolog_hp) = OPTS.autolog_hp {
        info!("Will automatically logout and quit, when getting to or below {autolog_hp} HP or popping a totem.");
    }

    if OPTS.no_stasis {
        info!("Will not perform any stasis duties!");
    }

    if OPTS.enable_pos_command {
        info!("The command !pos has been enabled for admins!");
    }

    if OPTS.auto_eat {
        info!("Automatic Eating is enabled.");
    }

    info!("Admins: {}", OPTS.admin.join(", "));

    info!("Logging in...");
    let mut account = if let Some(offline_username) = &OPTS.offline_username {
        info!("Using an offline account with username {offline_username:?}!");
        Account::offline(&offline_username)
    } else {
        let auth_result = azalea::auth::auth(
            "default",
            azalea::auth::AuthOpts {
                cache_file: Some(OPTS.auth_file.clone()),
                ..Default::default()
            },
        )
        .await?;
        azalea::Account {
            username: auth_result.profile.name,
            access_token: Some(Arc::new(Mutex::new(auth_result.access_token))),
            uuid: Some(auth_result.profile.id),
            account_opts: azalea::AccountOpts::Microsoft {
                email: "default".to_owned(),
            },
            // we don't do chat signing by default unless the user asks for it
            certs: None,
        }
    };

    if OPTS.sign_chat {
        account
            .request_certs()
            .await
            .context("Request certs for chat signing")?;
        info!("Chat signing is enabled. Retreived certs for it.");
    }

    info!(
        "Logged in as {}. Connecting to \"{}\"...",
        account.username, OPTS.server_address
    );

    // Read input and put in queue
    if std::io::stdin().is_terminal() {
        std::thread::spawn(|| loop {
            let mut line = String::new();
            if let Err(err) = std::io::stdin().read_line(&mut line) {
                warn!("Not accepting any input anymore because reading failed: Err: {err}");
                return;
            }
            let line: &str = line.trim();

            INPUTLINE_QUEUE.lock().push_back(line.to_owned());
        });
    }

    let mut builder = azalea::swarm::SwarmBuilder::new()
        .set_handler(handle)
        .set_swarm_handler(swarm_handle)
        .add_account(account.clone());
    if let Some(via) = &OPTS.via {
        info!("Using ViaProxy to translate the protocol to minecraft version {via}...");
        builder = builder.add_plugins(azalea_viaversion::ViaVersionPlugin::start(via).await);
    }
    if OPTS.openauthmod {
        info!("Using OpenAuthMod authentication protocol. If you connect over a proxy, it can see this traffic!");
        builder = builder.add_plugins(plugins::openauthmod::OpenAuthModPlugin::default());
    }
    // if OPTS.no_fall:
    // builder = builder.add_plugins(plugins::nofall::NoFallPlugin::default());
    builder
        .start(OPTS.server_address.as_str())
        .await
        .context("Running bot")?
}

#[derive(Default, Clone, Component)]
pub struct BotState {
    remembered_trapdoor_positions: Arc<Mutex<HashMap<String, BlockPos>>>,
    pathfinding_requested_by: Arc<Mutex<Option<String>>>,
    return_to_after_pulled: Arc<Mutex<Option<azalea::Vec3>>>,
    last_dm_handled_at: Arc<Mutex<Option<Instant>>>,
    eating_until_nutrition_over: Arc<Mutex<Option<u32>>>,
}

impl BotState {
    pub fn remembered_trapdoor_positions_path() -> PathBuf {
        PathBuf::from("remembered-trapdoor-positions.json")
    }

    pub async fn load_stasis(&mut self) -> Result<()> {
        let remembered_trapdoor_positions_path = Self::remembered_trapdoor_positions_path();
        if remembered_trapdoor_positions_path.exists()
            && !remembered_trapdoor_positions_path.is_dir()
        {
            *self.remembered_trapdoor_positions.lock() = serde_json::from_str(
                &tokio::fs::read_to_string(remembered_trapdoor_positions_path)
                    .await
                    .context("Read remembered_trapdoor_positions file")?,
            )
            .context("Parsing remembered_trapdoor_positions content")?;
            info!(
                "Loaded {} remembered trapdoor positions from file.",
                self.remembered_trapdoor_positions.lock().len()
            );
        } else {
            *self.remembered_trapdoor_positions.lock() = Default::default();
            warn!("File for rememembered trapdoor positions doesn't exist, yet.");
        };

        Ok(())
    }

    pub async fn save_stasis(&self) -> Result<()> {
        let json =
            serde_json::to_string_pretty(&*self.remembered_trapdoor_positions.as_ref().lock())
                .context("Convert remembered_trapdoor_positions to json")?;
        tokio::fs::write(Self::remembered_trapdoor_positions_path(), json)
            .await
            .context("Save remembered_trapdoor_positions as file")?;
        Ok(())
    }
}

async fn handle(mut bot: Client, event: Event, mut bot_state: BotState) -> anyhow::Result<()> {
    match event {
        Event::Login => {
            if !OPTS.no_stasis {
                info!("Loading remembered trapdoor positions...");
                bot_state.load_stasis().await?;
            }
        }
        Event::Chat(packet) => {
            info!(
                "CHAT: {}",
                if OPTS.no_color {
                    packet.message().to_string()
                } else {
                    packet.message().to_ansi()
                }
            );
            let message = packet.message().to_string();

            // Very security and sane way to find out, if message was a dm to self.
            // Surely no way to cheese it..
            let mut dm = None;
            if message.starts_with('[') && message.contains("-> me] ") {
                dm = Some((
                    message.split(" ").next().unwrap()[1..].to_owned(),
                    message.split("-> me] ").collect::<Vec<_>>()[1].to_owned(),
                ));
            } else if message.contains(" whispers to you: ") {
                dm = Some((
                    message.split(" ").next().unwrap().to_owned(),
                    message.split("whispers to you: ").collect::<Vec<_>>()[1].to_owned(),
                ));
            }

            if let Some((sender, content)) = dm {
                let (command, args) = if content.contains(' ') {
                    let mut all_args: Vec<_> = content.split(' ').map(|s| s.to_owned()).collect();
                    let command = all_args.remove(0);
                    (command, all_args)
                } else {
                    (content, vec![])
                };

                if bot_state
                    .last_dm_handled_at
                    .lock()
                    .map(|at| at.elapsed() > Duration::from_secs(1))
                    .unwrap_or(true)
                {
                    info!("Executing command {command:?} sent by {sender:?} with args {args:?}");
                    if commands::execute(&mut bot, &bot_state, sender, command, args)
                        .context("Executing command")?
                    {
                        *bot_state.last_dm_handled_at.lock() = Some(Instant::now());
                    } else {
                        warn!("Command was not executed. Most likely an unknown command.");
                    }
                } else {
                    warn!("Last command was handled less than a second ago. Ignoring command from {sender:?} to avoid getting spam kicked.");
                }
            }
        }
        Event::Packet(packet) => match packet.as_ref() {
            ClientboundGamePacket::AddEntity(packet) => {
                if !OPTS.no_stasis && packet.entity_type == EntityKind::EnderPearl {
                    let owning_player_entity_id = packet.data;
                    let mut bot = bot.clone();
                    let entity = bot.entity_by::<With<Player>, (&MinecraftEntityId,)>(
                        |(entity_id,): &(&MinecraftEntityId,)| {
                            entity_id.0 as i32 == owning_player_entity_id
                        },
                    );
                    if let Some(entity) = entity {
                        let game_profile = bot.entity_component::<GameProfileComponent>(entity);
                        info!(
                            "{} threw an EnderPearl at {}",
                            game_profile.name, packet.position
                        );
                        let mut found_trapdoor = None;

                        {
                            let world = bot.world();
                            let world = world.read();
                            for y_offset in [0, -1, 1, -2, 2, -3, 3, -4, 4, -5, 5] {
                                let azalea_block_pos = azalea::BlockPos::new(
                                    packet.position.x.floor() as i32,
                                    packet.position.y.floor() as i32 + y_offset,
                                    packet.position.z.floor() as i32,
                                );
                                if let Some(state) = world.get_block_state(&azalea_block_pos) {
                                    let block = Box::<dyn Block>::from(state);
                                    if block.id().ends_with("_trapdoor") {
                                        info!(
                                            "Detected trapdoor at {} for pearl thrown by {}",
                                            azalea_block_pos, game_profile.name
                                        );
                                        found_trapdoor = Some(BlockPos::from(azalea_block_pos));
                                        break;
                                    }
                                }
                            }
                        }

                        if let Some(block_pos) = found_trapdoor {
                            if bot_state
                                .remembered_trapdoor_positions
                                .lock()
                                .get(&game_profile.name)
                                != Some(&block_pos)
                            {
                                let mut remembered_trapdoor_positions =
                                    bot_state.remembered_trapdoor_positions.lock();
                                // Remove postions at same trapdoor
                                for playername in remembered_trapdoor_positions
                                    .keys()
                                    .map(|p| p.to_owned())
                                    .collect::<Vec<_>>()
                                {
                                    if remembered_trapdoor_positions.get(&playername)
                                        == Some(&block_pos)
                                    {
                                        info!("Found that {playername} is already using that trapdoor position. Removed that player!");
                                        remembered_trapdoor_positions.remove(&playername);
                                    }
                                }
                                remembered_trapdoor_positions
                                    .insert(game_profile.name.clone(), block_pos);

                                if !OPTS.quiet {
                                    bot.send_command_packet(&format!("msg {} You have thrown a pearl. Message me \"tp\" to get back here.", game_profile.name));
                                }
                                let bot_state = bot_state.clone();
                                tokio::spawn(async move {
                                    match bot_state
                                    .save_stasis()
                                    .await {
                                        Ok(_) => info!("Saved remembered trapdoor positions to file."),
                                        Err(err) => error!("Failed to save remembered trapdoor positions to file: {err:?}"),
                                    }
                                });
                            }
                        }
                    }
                }
                if packet.entity_type == EntityKind::Player {
                    /*info!(
                        "A player appeared at {} with entity id {}",
                        packet.position, packet.id
                    );*/
                }
            }
            ClientboundGamePacket::EntityEvent(packet) => {
                let my_entity_id = bot.entity_component::<MinecraftEntityId>(bot.entity).0;
                if packet.entity_id == my_entity_id && packet.event_id == 35 {
                    // Totem popped!
                    info!("I popped a Totem!");
                    if OPTS.autolog_hp.is_some() {
                        warn!("Disconnecting and quitting because --autolog-hp is enabled...");
                        bot.disconnect();
                        std::process::exit(69);
                    }
                }
            }
            ClientboundGamePacket::SetHealth(packet) => {
                info!(
                    "Health: {:.02}, Food: {:.02}, Saturation: {:.02}",
                    packet.health, packet.food, packet.saturation
                );
                if let Some(hp) = OPTS.autolog_hp {
                    if packet.health <= hp {
                        warn!("My Health got below {hp:.02}! Disconnecting and quitting...");
                        bot.disconnect();
                        std::process::exit(69);
                    }
                }

                // TODO: Use Attribute::GenericMaxHealth instead of hardcoded 20
                let eat_until_nutrition_over = bot_state
                    .eating_until_nutrition_over
                    .as_ref()
                    .lock()
                    .clone();
                if let Some(eat_until_nutrition_over) = eat_until_nutrition_over {
                    // Is eating
                    if packet.food > eat_until_nutrition_over {
                        // Food increased, stop eating
                        bot.ecs.lock().send_event(SendPacketEvent {
                            entity: bot.entity,
                            packet: ServerboundGamePacket::PlayerAction(ServerboundPlayerActionPacket {
                                action: azalea::protocol::packets::game::serverbound_player_action_packet::Action::ReleaseUseItem,
                                pos: Default::default(),
                                direction: Direction::Down,
                                sequence: 0,
                            })
                        });
                        *bot_state.eating_until_nutrition_over.lock() = None;
                        info!("Finished eating.");
                    }
                }

                if OPTS.auto_eat
                    && eat_until_nutrition_over.is_none()
                    && (packet.food <= 20 - (3 * 2) || (packet.health < 20f32 && packet.food < 20))
                {
                    let mut eat_item = None;

                    // Find first food item in hotbar
                    let inv = bot.entity_component::<InventoryComponent>(bot.entity);
                    let inv_menu = inv.inventory_menu;
                    for (hotbar_slot, slot) in inv_menu.hotbar_slots_range().enumerate() {
                        let item = inv_menu.slot(slot);
                        if let Some(ItemSlot::Present(item_slot)) = item {
                            if item_slot
                                .components
                                .get(azalea::registry::DataComponentKind::Food)
                                .is_some()
                                || FOOD_ITEMS.contains(&item_slot.kind)
                            {
                                eat_item = Some((
                                    hotbar_slot as u8,
                                    format!("{} ({}x)", item_slot.kind, item_slot.count),
                                ));
                                break;
                            }
                        }
                    }

                    if let Some((eat_hotbar_slot, eat_item_name)) = eat_item {
                        // Switch to slot and start eating
                        let entity = bot.entity;
                        let mut ecs = bot.ecs.lock();
                        ecs.send_event(SetSelectedHotbarSlotEvent {
                            entity,
                            slot: eat_hotbar_slot,
                        });
                        // In case that the slot differed, the packet was not sent, yet.
                        ecs.send_event_batch([
                            SendPacketEvent {
                                entity,
                                packet: ServerboundGamePacket::SetCarriedItem(
                                    ServerboundSetCarriedItemPacket {
                                        slot: eat_hotbar_slot as u16,
                                    },
                                ),
                            },
                            SendPacketEvent {
                                entity,
                                packet: ServerboundGamePacket::UseItem(ServerboundUseItemPacket {
                                    hand: InteractionHand::MainHand,
                                    sequence: 0,
                                }),
                            },
                        ]);
                        *bot_state.eating_until_nutrition_over.lock() = Some(packet.food);
                        info!("Eating {eat_item_name} in hotbar slot {eat_hotbar_slot}...");
                    }
                }
            }
            _ => {}
        },
        Event::Tick => {
            // Execute commands from input queue
            {
                let mut queue = INPUTLINE_QUEUE.lock();
                while let Some(line) = queue.pop_front() {
                    if line.starts_with("/") {
                        info!("Sending command: {line}");
                        bot.send_command_packet(&format!("{}", &line[1..]));
                    } else {
                        info!("Sending chat message: {line}");
                        bot.send_chat_packet(&line);
                    }
                }
            }

            // Look at players
            if let Some(max_dist) = OPTS.look_at_players {
                let is_pathfinding = {
                    let mut ecs = bot.ecs.lock();
                    let pathfinder: &Pathfinder = ecs
                        .query::<&Pathfinder>()
                        .get_mut(&mut *ecs, bot.entity)
                        .unwrap();
                    pathfinder.goal.is_some()
                };

                if !is_pathfinding {
                    let my_entity_id = bot.entity_component::<MinecraftEntityId>(bot.entity).0;
                    let my_pos = bot.entity_component::<Position>(bot.entity);
                    let my_eye_height = *bot.entity_component::<EyeHeight>(bot.entity) as f64;
                    let my_eye_pos = *my_pos + Vec3::new(0f64, my_eye_height, 0f64);

                    let mut closest_eye_pos = None;
                    let mut closest_dist_sqrt = f64::MAX;
                    let mut query =
                        bot.ecs
                            .lock()
                            .query::<(&Player, &Position, &EyeHeight, &Pose, &MinecraftEntityId)>();
                    for (_player, pos, eye_height, pose, entity_id) in query.iter(&bot.ecs.lock()) {
                        if entity_id.0 == my_entity_id {
                            continue;
                        }

                        let y_offset = match pose {
                            Pose::FallFlying | Pose::Swimming | Pose::SpinAttack => 0.5f64,
                            Pose::Sleeping => 0.25f64,
                            Pose::Sneaking => (**eye_height as f64) * 0.85,
                            _ => **eye_height as f64,
                        };
                        let eye_pos = **pos + Vec3::new(0f64, y_offset, 0f64);
                        let dist_sqrt = my_eye_pos.distance_to_sqr(pos);
                        if (closest_eye_pos.is_none() || dist_sqrt < closest_dist_sqrt)
                            && dist_sqrt <= (max_dist * max_dist) as f64
                        {
                            closest_eye_pos = Some(eye_pos);
                            closest_dist_sqrt = dist_sqrt;
                        }
                    }

                    if let Some(eye_pos) = closest_eye_pos {
                        bot.look_at(eye_pos);
                    }
                }
            }

            let mut pathfinding_requested_by = bot_state.pathfinding_requested_by.lock();
            if let Some(ref requesting_player) = *pathfinding_requested_by {
                let mut ecs = bot.ecs.lock();
                let pathfinder: &Pathfinder = ecs
                    .query::<&Pathfinder>()
                    .get_mut(&mut *ecs, bot.entity)
                    .unwrap();

                if !pathfinder.is_calculating && pathfinder.goal.is_none() {
                    drop(ecs);

                    if let Some(trapdoor_pos) = bot_state
                        .remembered_trapdoor_positions
                        .lock()
                        .remove(requesting_player)
                    {
                        if !OPTS.quiet {
                            bot.send_command_packet(&format!(
                                "msg {requesting_player} Welcome back, {requesting_player}!"
                            ));
                        }
                        bot.ecs.lock().send_event(SendPacketEvent {
                            entity: bot.entity,
                            packet: ServerboundGamePacket::UseItemOn(ServerboundUseItemOnPacket {
                                block_hit: BlockHit {
                                    block_pos: azalea::BlockPos::from(trapdoor_pos),
                                    direction: Direction::Down,
                                    location: azalea::Vec3 {
                                        x: trapdoor_pos.x as f64 + 0.5,
                                        y: trapdoor_pos.y as f64 + 0.5,
                                        z: trapdoor_pos.z as f64 + 0.5,
                                    },
                                    inside: true,
                                },
                                hand: InteractionHand::MainHand,
                                sequence: 0,
                            }),
                        });

                        *pathfinding_requested_by = None;
                        if let Some(return_to_after_pulled) =
                            bot_state.return_to_after_pulled.lock().take()
                        {
                            info!("Returning to {return_to_after_pulled}...");
                            let goal = BlockPosGoal(azalea::BlockPos {
                                x: return_to_after_pulled.x.floor() as i32,
                                y: return_to_after_pulled.y.floor() as i32,
                                z: return_to_after_pulled.z.floor() as i32,
                            });
                            if OPTS.no_mining {
                                bot.goto_without_mining(goal);
                            } else {
                                bot.goto(goal);
                            }
                        }

                        let bot_state = bot_state.clone();
                        tokio::spawn(async move {
                            match bot_state.save_stasis().await {
                                Ok(_) => info!("Saved remembered trapdoor positions to file."),
                                Err(err) => error!(
                                    "Failed to save remembered trapdoor positions to file: {err:?}"
                                ),
                            }
                        });
                    }
                }
            }
        }
        _ => {}
    }

    Ok(())
}

#[derive(Default, Clone, Component, Resource)]
pub struct SwarmState {}

async fn swarm_rejoin(mut swarm: Swarm, state: SwarmState, account: Account, join_opts: JoinOpts) {
    let mut reconnect_after_secs = 5;
    loop {
        info!("Reconnecting after {} seconds...", reconnect_after_secs);

        tokio::time::sleep(Duration::from_secs(reconnect_after_secs)).await;
        reconnect_after_secs = (reconnect_after_secs * 2).min(60 * 30); // 2x or max 30 minutes

        info!("Joining again...");
        match swarm
            .add_with_opts(&account, state.clone(), &join_opts)
            .await
        {
            Ok(_) => return,
            Err(join_err) => error!("Failed to rejoin: {join_err}"), // Keep rejoining
        }
    }
}

async fn swarm_handle(swarm: Swarm, event: SwarmEvent, state: SwarmState) -> anyhow::Result<()> {
    match event {
        SwarmEvent::Disconnect(account, join_opts) => {
            tokio::spawn(swarm_rejoin(
                swarm.clone(),
                state.clone(),
                (*account).clone(),
                join_opts.clone(),
            ));
        }
        _ => {}
    }

    Ok(())
}
