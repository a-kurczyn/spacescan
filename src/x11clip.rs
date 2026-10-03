//! The X11 clipboard, for files: offering them in several formats at once
//! (as file managers expect) and reading one format back as raw bytes.

use std::time::{Duration, Instant};
use x11rb::connection::{Connection, RequestConnection as _};
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Property,
    SelectionNotifyEvent, SelectionRequestEvent, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;

/// How long to wait for the clipboard's owner to answer.
const WAIT: Duration = Duration::from_secs(1);

/// An invisible window to talk to the clipboard through.
fn hidden_window(
    conn: &impl Connection,
    screen: usize,
    events: EventMask,
) -> Option<x11rb::protocol::xproto::Window> {
    let root = conn.setup().roots.get(screen)?.root;
    let win = conn.generate_id().ok()?;
    conn.create_window(
        0,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new().event_mask(events),
    )
    .ok()?;
    Some(win)
}

fn atom(conn: &impl Connection, name: &str) -> Option<Atom> {
    Some(
        conn.intern_atom(false, name.as_bytes())
            .ok()?
            .reply()
            .ok()?
            .atom,
    )
}

/// Makes the app the clipboard's owner, offering `formats` (name, data)
/// until another program takes the clipboard. False if that failed.
pub(crate) fn offer(formats: Vec<(&'static str, Vec<u8>)>) -> bool {
    let Ok((conn, screen)) = x11rb::connect(None) else {
        return false;
    };
    let Some(win) = hidden_window(&conn, screen, EventMask::NO_EVENT) else {
        return false;
    };
    let (Some(clipboard), Some(targets)) = (atom(&conn, "CLIPBOARD"), atom(&conn, "TARGETS"))
    else {
        return false;
    };
    let offered: Vec<(Atom, Vec<u8>)> = formats
        .into_iter()
        .filter_map(|(name, data)| Some((atom(&conn, name)?, data)))
        .collect();
    if conn
        .set_selection_owner(win, clipboard, x11rb::CURRENT_TIME)
        .is_err()
        || conn.flush().is_err()
    {
        return false;
    }
    let owner = conn
        .get_selection_owner(clipboard)
        .ok()
        .and_then(|c| c.reply().ok());
    if owner.map(|o| o.owner) != Some(win) {
        return false;
    }
    // Data bigger than one request can carry is refused rather than sent
    // in pieces.
    let max = conn.maximum_request_bytes().saturating_sub(64);
    std::thread::spawn(move || {
        while let Ok(event) = conn.wait_for_event() {
            match event {
                Event::SelectionRequest(req) => {
                    answer(&conn, &req, targets, &offered, max);
                }
                Event::SelectionClear(_) => break,
                _ => {}
            }
        }
    });
    true
}

/// Answers a program asking for the clipboard in format `req.target`.
fn answer(
    conn: &impl Connection,
    req: &SelectionRequestEvent,
    targets: Atom,
    offered: &[(Atom, Vec<u8>)],
    max: usize,
) {
    // Old programs leave the property out: the format's name is used then.
    let property = if req.property == x11rb::NONE {
        req.target
    } else {
        req.property
    };
    let given = if req.target == targets {
        let mut list: Vec<Atom> = offered.iter().map(|(a, _)| *a).collect();
        list.push(targets);
        conn.change_property32(
            PropMode::REPLACE,
            req.requestor,
            property,
            AtomEnum::ATOM,
            &list,
        )
        .is_ok()
    } else {
        match offered.iter().find(|(a, _)| *a == req.target) {
            Some((format, data)) if data.len() <= max => conn
                .change_property8(PropMode::REPLACE, req.requestor, property, *format, data)
                .is_ok(),
            _ => false,
        }
    };
    let notify = SelectionNotifyEvent {
        response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: req.time,
        requestor: req.requestor,
        selection: req.selection,
        target: req.target,
        property: if given { property } else { x11rb::NONE },
    };
    let _ = conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify);
    let _ = conn.flush();
}

/// The clipboard's contents in format `format`, if its owner offers it.
pub(crate) fn read(format: &str) -> Option<Vec<u8>> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let win = hidden_window(&conn, screen, EventMask::PROPERTY_CHANGE)?;
    let clipboard = atom(&conn, "CLIPBOARD")?;
    let target = atom(&conn, format)?;
    let property = atom(&conn, "SPACEMAP_CLIPBOARD")?;
    let incr = atom(&conn, "INCR")?;
    conn.convert_selection(win, clipboard, target, property, x11rb::CURRENT_TIME)
        .ok()?;
    conn.flush().ok()?;
    let deadline = Instant::now() + WAIT;
    let next_event = || loop {
        match conn.poll_for_event() {
            Ok(Some(event)) => return Some(event),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            _ => return None,
        }
    };
    // The owner puts the data in our window's property and says so.
    loop {
        if let Event::SelectionNotify(n) = next_event()? {
            if n.property == x11rb::NONE {
                return None;
            }
            break;
        }
    }
    let take = || {
        conn.get_property(true, win, property, AtomEnum::ANY, 0, u32::MAX / 4)
            .ok()?
            .reply()
            .ok()
    };
    let first = take()?;
    if first.type_ != incr {
        return Some(first.value);
    }
    // Big data comes in pieces, each announced by a new property value,
    // until an empty one.
    let mut data = Vec::new();
    loop {
        match next_event()? {
            Event::PropertyNotify(p) if p.atom == property && p.state == Property::NEW_VALUE => {
                let piece = take()?;
                if piece.value.is_empty() {
                    return Some(data);
                }
                data.extend_from_slice(&piece.value);
            }
            _ => {}
        }
    }
}
