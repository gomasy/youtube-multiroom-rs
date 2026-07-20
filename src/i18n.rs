#[derive(Clone, Copy)]
pub enum Lang {
    En,
    Ja,
}

impl Lang {
    pub fn from_env() -> Self {
        match std::env::var("APP_LANG").ok().as_deref() {
            Some(s) if s.starts_with("en") => Lang::En,
            Some(s) if s.starts_with("ja") => Lang::Ja,
            _ => Lang::Ja,
        }
    }

    // Alexa speech responses

    pub fn alexa_not_understood(self) -> &'static str {
        match self {
            Lang::En => "Sorry, I didn't understand that.",
            Lang::Ja => "すみません、よくわかりませんでした。",
        }
    }

    pub fn alexa_connected(self) -> &'static str {
        match self {
            Lang::En => {
                "Connected to YouTube MultiRoom. You can control playback from the web interface."
            }
            Lang::Ja => "YouTube マルチルームに接続しました。Web 画面から操作できます。",
        }
    }

    pub fn alexa_no_queued_track(self) -> &'static str {
        match self {
            Lang::En => "No track is queued. Please select a track from the web interface.",
            Lang::Ja => "再生する曲がキューされていません。Web 画面で曲を選んでください。",
        }
    }

    pub fn alexa_no_track(self) -> &'static str {
        match self {
            Lang::En => "There is no track to play.",
            Lang::Ja => "再生する曲がありません。",
        }
    }

    pub fn alexa_no_next(self) -> &'static str {
        match self {
            Lang::En => "There is no next track.",
            Lang::Ja => "次の曲がありません。",
        }
    }

    pub fn alexa_no_prev(self) -> &'static str {
        match self {
            Lang::En => "There is no previous track.",
            Lang::Ja => "前の曲がありません。",
        }
    }

    pub fn alexa_help(self) -> &'static str {
        match self {
            Lang::En => "Paste a YouTube URL in the web interface and press the play button. \
                         Then say \"Alexa, open YouTube Player\" on this device.",
            Lang::Ja => "Web ブラウザの操作画面で YouTube の URL を貼り付け、\
                         再生ボタンを押してください。\
                         その後、このデバイスで「アレクサ、YouTube プレーヤーを開いて」\
                         と言ってください。",
        }
    }

    pub fn alexa_use_web(self) -> &'static str {
        match self {
            Lang::En => "Please use the web interface to control playback.",
            Lang::Ja => "Web 画面から操作してください。",
        }
    }

    // API response messages

    pub fn api_play_queued(self) -> &'static str {
        match self {
            Lang::En => "Playback queued. Say \"Alexa, open YouTube Player\" on each Echo device.",
            Lang::Ja => "再生をキューしました。各 Echo で「アレクサ、YouTube プレーヤーを開いて」と言ってください",
        }
    }

    pub fn api_added_to_playlist(self, title: &str) -> String {
        match self {
            Lang::En => format!("Added \"{title}\" to playlist"),
            Lang::Ja => format!("「{title}」をプレイリストに追加しました"),
        }
    }

    pub fn api_queued_next(self, title: &str) -> String {
        match self {
            Lang::En => format!("Added \"{title}\" to Up Next"),
            Lang::Ja => format!("「{title}」を次に再生に追加しました"),
        }
    }
}
