//! Voice settings are loaded on demand. READY only needs the existing voice bitrate and limit.

use super::{model, Discord, Refusal};
use protocol::{ConversationKind, DiscordChannelVoice, DiscordVoiceRegion, DiscordVoiceSettings};
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
struct ListedRegion {
    id: String,
    name: String,
    #[serde(default)]
    deprecated: bool,
}

#[derive(Deserialize)]
struct ChannelVoice {
    bitrate: u32,
    #[serde(default)]
    user_limit: u32,
    rtc_region: Option<String>,
    video_quality_mode: Option<u8>,
}

fn endpoint_region(endpoint: &str) -> Option<String> {
    let host = endpoint.trim_start_matches("wss://").split('.').next()?;
    let region = host.trim_end_matches(|ch: char| ch.is_ascii_digit()).trim_end_matches('-');
    (!region.is_empty() && region != host).then(|| region.to_owned())
}

impl Discord {
    fn voice_regions(&self) -> Result<Vec<DiscordVoiceRegion>, Refusal> {
        const TTL: Duration = Duration::from_secs(3600);
        if let Some((_, regions)) = self.state.lock().unwrap().voice.regions.as_ref().filter(|(at, _)| at.elapsed() < TTL) {
            return Ok(regions.clone());
        }
        let raw = self.call("GET", "/voice/regions", None).map_err(super::super::refusal)?;
        let reported: Vec<ListedRegion> = serde_json::from_str(&raw)
            .map_err(|_| Refusal::Refused("Discord did not return a voice region list.".into()))?;
        let mut regions: Vec<_> = reported.into_iter()
            .filter(|region| !region.deprecated && !region.id.is_empty())
            .take(64)
            .map(|region| DiscordVoiceRegion { id: region.id, name: region.name })
            .collect();
        regions.sort_by(|a, b| a.name.cmp(&b.name));
        self.state.lock().unwrap().voice.regions = Some((Instant::now(), regions.clone()));
        Ok(regions)
    }

    fn voice_bitrate_max(&self, guild: &str) -> u32 {
        let state = self.state.lock().unwrap();
        let tier = state.guilds.get(guild).map_or(0, |guild| {
            if guild.features.iter().any(|feature| feature == "VIP_REGIONS") { 3 } else { guild.premium_tier }
        });
        match tier {
            0 => 96_000,
            1 => 128_000,
            2 => 256_000,
            _ => 384_000,
        }
    }

    pub(in crate::adapters::discord) fn discord_voice_settings(&self, channel: &str) -> Result<DiscordVoiceSettings, Refusal> {
        let row = self.ctx.known(channel).ok().flatten()
            .ok_or_else(|| Refusal::Refused("This conversation is no longer available.".into()))?;
        let (current_region, direct) = {
            let state = self.state.lock().unwrap();
            let endpoint = state.voice.call.as_ref().filter(|call| call.conversation == channel)
                .and_then(|call| call.server.as_ref()).and_then(|(endpoint, _)| endpoint_region(endpoint));
            let direct = row.kind.is_direct() && state.voice.calls.contains(channel);
            let region = endpoint.or_else(|| state.voice.call_regions.get(channel).cloned());
            (region, direct)
        };
        let guild = row.space.as_deref().filter(|_| row.kind == ConversationKind::Voice && row.rights.rename);
        let (configuration, max_bitrate) = if let Some(guild) = guild {
            let raw = self.call("GET", &format!("/channels/{channel}"), None).map_err(super::super::refusal)?;
            let wire: ChannelVoice = serde_json::from_str(&raw)
                .map_err(|_| Refusal::Refused("Discord did not return this voice channel's settings.".into()))?;
            let config = DiscordChannelVoice {
                bitrate: wire.bitrate,
                user_limit: wire.user_limit,
                rtc_region: wire.rtc_region,
                video_quality_mode: wire.video_quality_mode.unwrap_or(1),
            };
            let max = self.voice_bitrate_max(guild).max(config.bitrate);
            (Some(config), max)
        } else {
            (None, 0)
        };
        let regions = if guild.is_some() || direct { self.voice_regions()? } else { Vec::new() };
        Ok(DiscordVoiceSettings { channel: configuration, max_bitrate, current_region, regions })
    }

    pub(in crate::adapters::discord) fn edit_discord_voice_channel(&self, channel: &str, edit: DiscordChannelVoice) -> Result<(), Refusal> {
        let guild = self.ctx.known(channel).ok().flatten().filter(|row| row.kind == ConversationKind::Voice)
            .and_then(|row| row.space).ok_or_else(|| Refusal::Refused("This server voice channel is no longer available.".into()))?;
        self.require_channel_perm(&guild, channel, model::MANAGE_CHANNELS, "edit this voice channel")?;
        let max = self.voice_bitrate_max(&guild).max(self.channel_bps(&guild, channel));
        if !(8_000..=max).contains(&edit.bitrate) || edit.user_limit > 99 || !matches!(edit.video_quality_mode, 1 | 2) {
            return Err(Refusal::Refused(format!("Use a bitrate from 8 to {} kb/s, a user limit from 0 to 99, and an available video quality.", max / 1000)));
        }
        if edit.rtc_region.as_ref().is_some_and(|region| region.is_empty() || region.len() > 64) {
            return Err(Refusal::Refused("Choose an available voice region.".into()));
        }
        let raw = self.call("PATCH", &format!("/channels/{channel}"), Some(json!(edit))).map_err(super::super::refusal)?;
        let mut channel: model::Channel = serde_json::from_str(&raw)
            .map_err(|_| Refusal::Refused("Discord did not return the updated channel.".into()))?;
        channel.guild_id.get_or_insert(guild);
        self.channel_changed(channel).map_err(super::super::manage::failed)
    }

    pub(in crate::adapters::discord) fn set_discord_call_region(&self, channel: &str, region: &str) -> Result<(), Refusal> {
        let direct = self.ctx.known(channel).ok().flatten().is_some_and(|row| row.kind.is_direct());
        if !direct || !self.state.lock().unwrap().voice.calls.contains(channel) {
            return Err(Refusal::Refused("Start or join a direct call to change its voice region.".into()));
        }
        if !self.voice_regions()?.iter().any(|option| option.id == region) {
            return Err(Refusal::Refused("Choose an available voice region.".into()));
        }
        self.call("PATCH", &format!("/channels/{channel}/call"), Some(json!({"region": region})))
            .map(|_| ()).map_err(super::super::refusal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_voice_endpoint_says_its_region_without_the_instance_number() {
        assert_eq!(endpoint_region("us-east-123.discord.media:2048").as_deref(), Some("us-east"));
        assert_eq!(endpoint_region("wss://rotterdam204.discord.media").as_deref(), Some("rotterdam"));
        assert_eq!(endpoint_region("127.0.0.1"), None);
    }
}
