use super::*;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakingPace {
    Slower,
    #[default]
    Normal,
    Faster,
}

impl SpeakingPace {
    pub fn instruction(self) -> &'static str {
        match self {
            Self::Slower => "Speaking pace preference: speak a little more slowly, with natural pauses and clear words.",
            Self::Normal => "Speaking pace preference: use a normal, natural conversational pace.",
            Self::Faster => "Speaking pace preference: speak a little faster, keeping words clear and pauses natural.",
        }
    }
}

// GPT Live has no numeric playback-speed field. Use its supported spoken
// instruction channel and report acceptance separately from saved preferences.
pub struct PaceUpdate {
    attempted: SpeakingPace,
    pending: Option<(String, Instant)>,
}
impl PaceUpdate {
    pub fn new(pace: SpeakingPace) -> Self { Self { attempted: pace, pending: None } }
    pub fn command(&mut self, desired: SpeakingPace) -> Option<Value> {
        if desired == self.attempted || self.pending.is_some() { return None; }
        self.attempted = desired;
        let id = format!("kindred-pace-{}", db::id());
        self.pending = Some((id.clone(), Instant::now()));
        Some(json!({"type":"session.instructions.append","event_id":id,"delegation_id":null,
            "content":format!("{} This replaces earlier speaking pace preferences. Apply it to your next reply. Do not acknowledge this setting, start a new task, repeat a result, or interrupt the current reply just to announce the change.",desired.instruction())}))
    }
    pub fn handle(&mut self, event: &Value) -> Option<Value> {
        let id = event["client_event_id"].as_str().or_else(||event["error"]["event_id"].as_str())?;
        if !self.pending.as_ref().is_some_and(|(pending,_)|pending==id) { return None; }
        match event["type"].as_str()? {
            "session.instructions.appended" => { self.pending=None; Some(json!({"type":"options_applied","pace":self.attempted})) }
            "error" => { self.pending=None; Some(Self::failure()) }
            _ => None,
        }
    }
    pub fn is_options_error(event: &Value) -> bool {
        event["type"]=="error" && event["client_event_id"].as_str().or_else(||event["error"]["event_id"].as_str()).is_some_and(|id|id.starts_with("kindred-pace-"))
    }
    pub fn timeout(&mut self) -> Option<Value> {
        if !self.pending.as_ref().is_some_and(|(_,at)|at.elapsed()>=Duration::from_secs(10)) { return None; }
        self.pending=None; Some(Self::failure())
    }
    fn failure() -> Value {
        json!({"type":"options_error","message":"Speaking pace was saved for your next call, but could not be applied to this call."})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pace_updates_are_coalesced_and_acked_without_restarting_voice() {
        let mut update=PaceUpdate::new(SpeakingPace::Normal);
        assert!(update.command(SpeakingPace::Normal).is_none());
        let command=update.command(SpeakingPace::Faster).unwrap();
        assert_eq!(command["type"],"session.instructions.append");
        assert!(command["content"].as_str().unwrap().contains("next reply"));
        assert!(update.command(SpeakingPace::Slower).is_none());
        assert!(update.handle(&json!({"type":"session.instructions.appended","client_event_id":"unrelated"})).is_none());
        let result=update.handle(&json!({"type":"session.instructions.appended","client_event_id":command["event_id"]})).unwrap();
        assert_eq!(result["pace"],"faster");
        assert!(update.command(SpeakingPace::Faster).is_none());
        assert!(update.command(SpeakingPace::Slower).is_some());
    }
    #[test]
    fn rejected_and_timed_out_options_do_not_retry_or_end_the_call() {
        let mut update=PaceUpdate::new(SpeakingPace::Normal);
        let command=update.command(SpeakingPace::Faster).unwrap();
        let error=json!({"type":"error","client_event_id":command["event_id"]});
        assert!(PaceUpdate::is_options_error(&error));
        assert_eq!(update.handle(&error).unwrap()["type"],"options_error");
        assert!(update.command(SpeakingPace::Faster).is_none());
        assert!(!PaceUpdate::is_options_error(&json!({"type":"error","client_event_id":"audio-1"})));
        let _=update.command(SpeakingPace::Slower).unwrap();
        update.pending.as_mut().unwrap().1=Instant::now()-Duration::from_secs(11);
        assert_eq!(update.timeout().unwrap()["type"],"options_error");
        assert!(update.timeout().is_none());
        assert!(update.command(SpeakingPace::Slower).is_none());
    }
}
