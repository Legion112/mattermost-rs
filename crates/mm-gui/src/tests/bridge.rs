use super::*;

/// The engine must not be able to express a conclusion. If a variant like
/// `ChannelBecameUnread` ever appears here, there are two places that know
/// what unread means and they will disagree.
#[test]
fn events_carry_server_payloads_rather_than_conclusions() {
    let rendered = format!(
        "{:?}",
        Event::Channels(vec![Channel {
            id: ChannelId::new("c1"),
            ..Channel::default()
        }])
    );
    assert!(rendered.contains("c1"));

    for event in [
        Event::Threading(Threading::Collapsed),
        Event::Teams(Vec::new()),
        Event::Members(Vec::new()),
        Event::Users(Vec::new()),
        Event::Status("working".into()),
        Event::Failed("boom".into()),
        Event::ResyncRequired("gap".into()),
    ] {
        assert!(!format!("{event:?}").is_empty());
        assert!(!format!("{:?}", event.clone()).is_empty());
    }
}

#[test]
fn commands_round_trip_through_debug_and_clone() {
    for command in [
        Command::Sync,
        Command::OpenChannel(ChannelId::new("c1")),
        Command::Send {
            root: None,
            channel: ChannelId::new("c1"),
            text: "hello".into(),
            attach: Vec::new(),
        },
        Command::MarkRead(ChannelId::new("c1")),
        Command::Quit,
    ] {
        assert!(!format!("{command:?}").is_empty());
        assert!(!format!("{:?}", command.clone()).is_empty());
    }
}

#[test]
fn connected_and_posts_carry_what_the_gui_needs() {
    let connected = Event::Connected {
        server: "https://example.invalid".into(),
        me: UserId::new("u1"),
        username: "jane".into(),
    };
    assert!(format!("{connected:?}").contains("jane"));

    let posts = Event::Posts {
        channel: ChannelId::new("c1"),
        page: PostList::default(),
    };
    assert!(format!("{posts:?}").contains("c1"));
}

/// A pasted screenshot carries its bytes; a picked file carries its
/// path. Writing the paste to a temporary file would mean one file in
/// the cache per paste and something, somewhere, remembering to delete
/// it -- a leak with extra steps.
#[test]
fn an_attachment_names_itself_whichever_kind_it_is() {
    let picked = Attachment::File(std::path::PathBuf::from("/home/me/report.pdf"));
    assert_eq!(picked.name(), "report.pdf");

    let pasted = Attachment::Pasted {
        name: "pasted-17.png".to_owned(),
        data: vec![1, 2, 3],
    };
    assert_eq!(pasted.name(), "pasted-17.png");

    // A path with no file name at all still has something to call it,
    // because the server needs a name and refusing to send would be
    // the worse answer.
    assert_eq!(
        Attachment::File(std::path::PathBuf::from("/")).name(),
        "attachment"
    );

    assert_eq!(picked, picked.clone());
    assert!(!format!("{pasted:?}").is_empty());
}
