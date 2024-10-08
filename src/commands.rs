use azalea::{
    ecs::query::With,
    entity::{metadata::Player, Position},
    pathfinder::goals::{BlockPosGoal, ReachBlockPosGoal},
    prelude::*,
    world::InstanceName,
    GameProfileComponent, Vec3,
};

use crate::{BotState, ARGS};

#[allow(clippy::too_many_lines)]
pub fn execute(
    bot: &mut Client,
    bot_state: &BotState,
    sender: &str,
    mut command: String,
    args: &[String],
) -> anyhow::Result<bool> {
    if command.starts_with('!') {
        command.remove(0);
    }
    command = command.to_lowercase();
    let sender_is_admin = ARGS.admin.iter().any(|a| sender.eq_ignore_ascii_case(a));

    match command.as_str() {
        "help" => {
            let mut commands = vec!["!help", "!about"];
            if !ARGS.no_stasis {
                commands.push("!tp");
            }
            if sender_is_admin {
                commands.append(&mut vec!["!comehere", "!say", "!stop"]);
                if ARGS.enable_pos_command {
                    commands.push("!pos");
                }
            }
            if !ARGS.admin.is_empty() {
                commands.push("!admins");
            }
            commands.sort_unstable();

            send_command(
                bot,
                &format!("msg {sender} Commands: {}", commands.join(", ")),
            );
            Ok(true)
        }
        "about" => {
            send_command(bot, &format!("msg {sender} Hi, I'm running EnderKill98's azalea-based stasis-bot {}: github.com/EnderKill98/stasis-bot", env!("CARGO_PKG_VERSION")));
            Ok(true)
        }
        "tp" => {
            if ARGS.no_stasis {
                send_command(
                    bot,
                    &format!("msg {sender} I'm not allowed to do pearl duties :(..."),
                );
                return Ok(true);
            }

            if let Some(trapdoor_pos) = bot_state.remembered_trapdoor_positions.lock().get(sender) {
                if bot_state.pathfinding_requested_by.lock().is_some() {
                    send_command(bot, &format!("msg {sender} Please ask again in a bit. I'm currently already going somewhere..."));
                    return Ok(true);
                }
                send_command(
                    bot,
                    &format!("msg {sender} Walking to your stasis chamber..."),
                );

                *bot_state.return_to_after_pulled.lock() =
                    Some(Vec3::from(&bot.entity_component::<Position>(bot.entity)));

                info!("Walking to {trapdoor_pos:?}...");
                let goal = ReachBlockPosGoal {
                    pos: azalea::BlockPos::from(*trapdoor_pos),
                    chunk_storage: bot.world().read().chunks.clone(),
                };
                if ARGS.no_mining {
                    bot.goto_without_mining(goal);
                } else {
                    bot.goto(goal);
                }
                *bot_state.pathfinding_requested_by.lock() = Some(sender.to_owned());
            } else {
                send_command(
                    bot,
                    &format!("msg {sender} I'm not aware whether you have a pearl here. Sorry!"),
                );
            }

            Ok(true)
        }
        "comehere" => {
            if !sender_is_admin {
                send_command(bot, &format!("msg {sender} Sorry, but you need to be specified as an admin to use this command!"));
                return Ok(true);
            }

            let sender_entity = bot.entity_by::<With<Player>, (&GameProfileComponent,)>(
                |(profile,): &(&GameProfileComponent,)| profile.name == sender,
            );
            if let Some(sender_entity) = sender_entity {
                let position = bot.entity_component::<Position>(sender_entity);
                #[allow(clippy::cast_possible_truncation)]
                let goal = BlockPosGoal(azalea::BlockPos {
                    x: position.x.floor() as i32,
                    y: position.y.floor() as i32,
                    z: position.z.floor() as i32,
                });
                if ARGS.no_mining {
                    bot.goto_without_mining(goal);
                } else {
                    bot.goto(goal);
                }
                send_command(
                    bot,
                    &format!("msg {sender} Walking to your block position..."),
                );
            } else {
                send_command(
                    bot,
                    &format!("msg {sender} I could not find you in my render distance!"),
                );
            }
            Ok(true)
        }
        "admins" => {
            send_command(
                bot,
                &format!("msg {sender} Admins: {}", ARGS.admin.join(", ")),
            );
            Ok(true)
        }
        "say" => {
            if !sender_is_admin {
                send_command(bot, &format!("msg {sender} Sorry, but you need to be specified as an admin to use this command!"));
                return Ok(true);
            }

            let command_or_chat = args.join(" ");
            if let Some(stripped) = command_or_chat.strip_prefix('/') {
                info!("Sending command: {command_or_chat}");
                bot.send_command_packet(stripped);
            } else {
                info!("Sending chat message: {command_or_chat}");
                bot.send_chat_packet(&command_or_chat);
            }
            Ok(true)
        }
        "stop" => {
            if !sender_is_admin {
                send_command(bot, &format!("msg {sender} Sorry, but you need to be specified as an admin to use this command!"));
                return Ok(true);
            }

            info!("Stopping... Bye!");
            std::process::exit(crate::EXITCODE_USER_REQUESTED_STOP);
        }
        "pos" => {
            if !sender_is_admin {
                send_command(bot, &format!("msg {sender} Sorry, but you need to be specified as an admin to use this command!"));
                return Ok(true);
            }
            if !ARGS.enable_pos_command {
                send_command(bot, &format!("msg {sender} Sorry, but this command was not enabled. The owner needs to add the flag --enable-pos-command in order to do so!"));
                return Ok(true);
            }

            let pos = bot.component::<Position>();
            let world_name = bot.component::<InstanceName>();
            send_command(
                bot,
                &format!(
                    "msg {sender} I'm at {:.03} {:.03} {:.03} in {}",
                    pos.x, pos.y, pos.z, world_name.path,
                ),
            );
            Ok(true)
        }

        _ => Ok(false), // Do nothing if unrecognized command
    }
}

pub fn send_command(bot: &mut Client, command: &str) {
    if ARGS.quiet {
        info!("Quiet mode: Supressed sending command: {command}");
    } else {
        info!("Sending command: {command}");
        bot.send_command_packet(command);
    }
}
