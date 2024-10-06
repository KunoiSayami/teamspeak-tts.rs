use std::sync::Arc;

use futures::{channel::oneshot, future, StreamExt as _, TryStreamExt as _};
use tap::TapOptional as _;
use tokio::{
    sync::{mpsc, Notify},
    task::JoinHandle,
};
use tsclientlib::{
    prelude::OutMessageTrait, ChannelId, ClientDbId, ClientId, Connection, DisconnectOptions,
    OutCommandExt, StreamItem,
};
use tsproto::Identity;
use tsproto_packets::packets::OutCommand;

use crate::{config::Config, tts::TeamSpeakEvent};

fn find_self_and_target(
    current_channel: Option<ChannelId>,
    state: &tsclientlib::data::Connection,
    target_id: Option<ClientId>,
    interest: Option<ClientDbId>,
) -> Option<(ClientId, ChannelId, OutCommand)> {
    fn make_out_message(client_id: ClientId, channel_id: ChannelId) -> OutCommand {
        tsclientlib::messages::c2s::OutClientMoveMessage::new(&mut std::iter::once(
            tsclientlib::messages::c2s::OutClientMovePart {
                client_id,
                channel_id,
                channel_password: None,
            },
        ))
    }

    if interest.is_none() || state.clients.is_empty() {
        return None;
    }

    let interest_db_id = interest.unwrap();

    let current_user = state.own_client;

    // Get current channel, if none, throw it
    let current_channel = current_channel
        .or_else(|| state.clients.get(&current_user).map(|x| x.channel))
        .tap_none(|| log::error!("Unable fetch current channel id"))?;

    // If found client in channel
    if let Some(client) = target_id.and_then(|client_id| state.clients.get(&client_id)) {
        let channel = client.channel;
        if channel == current_channel {
            return None;
        }
        return Some((client.id, channel, make_out_message(current_user, channel)));
    }

    // Not found
    for (client_id, client) in state.clients.iter() {
        // Check database id equal
        if client.database_id != interest_db_id {
            continue;
        }
        // Check channel equal
        if current_channel == client.channel {
            return None;
        }
        return Some((
            *client_id,
            client.channel,
            make_out_message(current_user, client.channel),
        ));
    }

    // Search completed by not found
    None
}

fn check_is_kick_event(client_id: ClientId, events: &[tsclientlib::events::Event]) -> bool {
    for event in events {
        if let tsclientlib::events::Event::PropertyRemoved {
            id: tsclientlib::events::PropertyId::Client(id),
            old: _,
            invoker,
            extra: _,
        } = event
        {
            if id == &client_id {
                if let Some(invoker) = invoker {
                    log::error!(
                        "Kicked by {}({})",
                        invoker.name,
                        invoker
                            .uid
                            .as_ref()
                            .map(|s| base64::display::Base64Display::new(
                                &s.0,
                                &base64::engine::general_purpose::STANDARD
                            )
                            .to_string())
                            .unwrap_or_else(|| "N/A".to_string())
                    )
                }
                return true;
            }
        }
    }
    false
}

pub struct ConnectionHandler {
    handle: JoinHandle<anyhow::Result<()>>,
}

impl ConnectionHandler {
    pub fn start(
        config: Config,
        verbose: u8,
        log_command: bool,
        receiver: mpsc::Receiver<TeamSpeakEvent>,
        override_server: Option<String>,
    ) -> anyhow::Result<(Self, oneshot::Receiver<()>)> {
        let teamspeak_options =
            Connection::build(override_server.unwrap_or_else(|| config.teamspeak().server()))
                .log_commands(verbose >= 5 || log_command)
                .log_packets(verbose >= 6)
                .log_udp_packets(verbose >= 7)
                .channel_id(tsclientlib::ChannelId(config.teamspeak().channel()))
                .version(tsclientlib::Version::Linux_3_5_7__3)
                .name(config.teamspeak().nickname().to_string())
                .output_muted(true)
                .output_hardware_enabled(false)
                .identity(Identity::new_from_str(config.teamspeak().identity())?);
        let handle = tokio::spawn(Self::run(
            teamspeak_options.connect()?,
            config.teamspeak().follow(),
            receiver,
        ));

        let (sender, exit_receiver) = oneshot::channel();
        Ok((
            Self {
                handle: tokio::spawn(Self::supervisor(sender, handle)),
            },
            exit_receiver,
        ))
    }

    async fn supervisor(
        sender: oneshot::Sender<()>,
        handle: JoinHandle<anyhow::Result<()>>,
    ) -> anyhow::Result<()> {
        let ret = handle.await;
        sender.send(()).ok();
        ret?
    }

    async fn run(
        mut conn: Connection,
        tail_target: Option<ClientDbId>,
        mut receiver: mpsc::Receiver<TeamSpeakEvent>,
    ) -> anyhow::Result<()> {
        if let Some(r) = conn
            .events()
            .try_filter(|e| future::ready(matches!(e, StreamItem::BookEvents(_))))
            .next()
            .await
        {
            r?;
        }

        tsclientlib::messages::c2s::OutChannelSubscribeAllMessage::new()
            .send(&mut conn)
            .unwrap();

        let mut tail_target_client = None;
        let mut current_channel = None;
        let client_id = conn.get_state().unwrap().own_client;

        let notifier = Arc::new(Notify::new());
        let (exit_notifier, mut exit_receiver) = mpsc::channel(1);

        let mut refresh = true;

        #[cfg(feature = "measure-time")]
        let mut start = tokio::time::Instant::now();
        let mut kicked = false;

        loop {
            if exit_receiver.try_recv().is_ok() {
                kicked = true;
                break;
            }
            if refresh && tail_target.is_some() {
                if let Some((client_id, channel_id, command)) = find_self_and_target(
                    current_channel,
                    conn.get_state().unwrap(),
                    tail_target_client,
                    tail_target,
                ) {
                    if !tail_target_client.replace(client_id).eq(&Some(client_id)) {
                        log::info!("Following client {client_id}");
                    }
                    current_channel.replace(channel_id);
                    command.send(&mut conn).unwrap();
                    log::debug!("Switching to channel: {channel_id}");
                }
            }

            let notify_waiter = notifier.clone();
            let events = conn.events().try_for_each(|event| {
                let notify = notifier.clone();
                let exit_notifier = exit_notifier.clone();
                async move {
                    match event {
                        StreamItem::BookEvents(event) => {
                            if check_is_kick_event(client_id, &event) {
                                exit_notifier.send(()).await.ok();
                                return Ok(());
                            }
                            notify.notify_waiters();
                        }
                        StreamItem::MessageEvent(_) => {
                            notify.notify_waiters();
                        }
                        _ => {}
                    }
                    Ok(())
                }
            });

            tokio::select! {
                send_audio = receiver.recv() => {
                    if let Some(packet) = send_audio {
                        match packet {
                            TeamSpeakEvent::Muted(_) => {
                                packet.to_packet().send(&mut conn)?;
                            },
                            TeamSpeakEvent::Data(packet) => conn.send_audio(packet)?,
                            TeamSpeakEvent::Exit => {
                                break;
                            }
                        }
                    } else {
                        log::info!("Audio sending stream was canceled");
                        break;
                    }
                    #[cfg(feature = "measure-time")]
                    {
                        let current = tokio::time::Instant::now();
                        log::trace!("{:?} elapsed to send audio", current - start);
                        start = current;
                    }
                }
                _ =  async move {
                    notify_waiter.notified().await;
                }, if tail_target.is_some() => {
                    refresh = true;
                }
                ret = events => {
                    ret?;
                }
            };
        }
        if !kicked {
            // Disconnect
            conn.disconnect(
                DisconnectOptions::new()
                    .message("User requested.")
                    .reason(tsclientlib::Reason::None),
            )?;
        }
        conn.events().for_each(|_| future::ready(())).await;
        Ok(())
    }

    pub async fn join(self) -> anyhow::Result<()> {
        self.handle.await?
    }
}
