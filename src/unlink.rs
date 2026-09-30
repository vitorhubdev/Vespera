//! What to do when WhatsApp drops this device.
//!
//! A network drop reconnects on its own. Losing the account does not: the
//! history stays, and the next link is a choice. These functions are the
//! decisions; the worker and the window only apply them.

use std::time::Duration;

/// Why the account session ended. A network drop is not one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndKind {
    /// The phone removed this device, or the server said it is logged out.
    Removed,
    /// Another WhatsApp Web session took this one.
    Replaced,
    /// WhatsApp blocked the account for a while.
    Banned,
    /// The linking code ran out, or connecting never reached a session.
    Expired,
    /// The server ended the session without a reason we recognise.
    Unknown,
}

/// A stored account and the one that just paired.
pub fn same_account(
    stored_pn: Option<&str>,
    stored_lid: Option<&str>,
    new_pn: &str,
    new_lid: &str,
) -> bool {
    let stored_pn = blank(stored_pn);
    let stored_lid = blank(stored_lid);
    if stored_pn.is_none() && stored_lid.is_none() {
        return true;
    }
    stored_pn.is_some_and(|id| id == new_pn) || stored_lid.is_some_and(|id| id == new_lid)
}

fn blank(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// `Connecting` without a live session is given up after this long.
pub const CONNECTING_LIMIT: Duration = Duration::from_secs(90);

/// A real network drop shows the long-offline line after this long.
pub const OFFLINE_NOTICE: Duration = Duration::from_secs(120);

/// Connecting with no live session has gone on too long.
pub fn connecting_gave_up(elapsed: Duration, session_live: bool) -> bool {
    !session_live && elapsed >= CONNECTING_LIMIT
}

/// Whether the next status is allowed to replace a held disconnect.
/// Pairing and a finished link may. `Connecting` may not: that is the
/// spinner that used to hide a logged-out device.
pub fn status_may_leave_hold(next: HoldNext) -> bool {
    matches!(
        next,
        HoldNext::Unlinked | HoldNext::Connected | HoldNext::Ended | HoldNext::Failed
    )
}

/// The statuses that matter for the hold. The worker maps its own enum here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldNext {
    Connecting,
    Disconnected,
    Unlinked,
    Connected,
    Ended,
    Failed,
    Other,
}

/// Line written for one link change. A QR payload never appears in it.
pub fn link_log(event: LinkLog) -> String {
    match event {
        LinkLog::Qr { index } => format!("QR gerado ({index}/6)"),
        LinkLog::CodeExpired => "QR esgotado".to_owned(),
        LinkLog::Unlinked => "Unlinked".to_owned(),
        LinkLog::PairingCode => "Unlinked (pairing code ready)".to_owned(),
        LinkLog::Ended(kind) => format!("desconectado ({})", kind_key(kind)),
        LinkLog::Other(text) => text.to_owned(),
    }
}

/// What the worker is allowed to write about the link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkLog {
    Qr { index: u32 },
    CodeExpired,
    Unlinked,
    PairingCode,
    Ended(EndKind),
    Other(&'static str),
}

pub fn kind_key(kind: EndKind) -> &'static str {
    match kind {
        EndKind::Removed => "removed",
        EndKind::Replaced => "replaced",
        EndKind::Banned => "banned",
        EndKind::Expired => "expired",
        EndKind::Unknown => "unknown",
    }
}

pub fn kind_from_key(key: &str) -> Option<EndKind> {
    Some(match key {
        "removed" => EndKind::Removed,
        "replaced" => EndKind::Replaced,
        "banned" => EndKind::Banned,
        "expired" => EndKind::Expired,
        "unknown" => EndKind::Unknown,
        _ => return None,
    })
}

/// `removed:1710000000` in the archive, so a restart still knows.
pub fn encode_end(kind: EndKind, at: i64) -> String {
    format!("{}:{at}", kind_key(kind))
}

pub fn decode_end(raw: &str) -> Option<(EndKind, i64)> {
    let (key, at) = raw.split_once(':')?;
    Some((kind_from_key(key)?, at.parse().ok()?))
}

/// A replaced session is never resumed by the worker.
pub fn replaced_reconnects() -> bool {
    false
}

/// `Connected` waits while the user is still choosing which account to keep.
pub fn accept_connected(choice_pending: bool) -> bool {
    !choice_pending
}

/// A session file already on disk is a linked device, even before the
/// socket reports `Connected`. An empty or missing file is not.
pub fn session_on_disk(bytes: u64) -> bool {
    bytes > 0
}

/// Words for the disconnect dialog. No emoji.
pub struct Notice {
    pub title: &'static str,
    pub reconnect: &'static str,
    pub pair: &'static str,
    pub later: &'static str,
    pub use_here: &'static str,
    pub readonly: &'static str,
    pub expired: &'static str,
    pub new_code: &'static str,
    pub other_title: &'static str,
    pub other_body: &'static str,
    pub wipe: &'static str,
    pub keep: &'static str,
    pub offline: &'static str,
    pub offline_long: &'static str,
    pub try_now: &'static str,
}

pub fn notice(locale: &str) -> Notice {
    match locale {
        "pt" => Notice {
            title: "Você foi desconectado do WhatsApp",
            reconnect: "Reconectar (ler QR)",
            pair: "Conectar pelo número",
            later: "Agora não",
            use_here: "Usar aqui",
            readonly: "Desconectado — somente leitura",
            expired: "O código expirou",
            new_code: "Gerar novo código",
            other_title: "Esta é outra conta",
            other_body: "Esta é outra conta. Apagar o histórico da conta anterior?",
            wipe: "Apagar o histórico",
            keep: "Manter o histórico",
            offline: "Sem conexão. Tentando de novo…",
            offline_long: "Sem conexão há",
            try_now: "Tentar agora",
        },
        "es" => Notice {
            title: "Te desconectaste de WhatsApp",
            reconnect: "Reconectar (leer QR)",
            pair: "Conectar con el número",
            later: "Ahora no",
            use_here: "Usar aquí",
            readonly: "Desconectado — solo lectura",
            expired: "El código expiró",
            new_code: "Generar un código nuevo",
            other_title: "Esta es otra cuenta",
            other_body: "Esta es otra cuenta. ¿Borrar el historial de la cuenta anterior?",
            wipe: "Borrar el historial",
            keep: "Conservar el historial",
            offline: "Sin conexión. Reintentando…",
            offline_long: "Sin conexión desde hace",
            try_now: "Reintentar ahora",
        },
        _ => Notice {
            title: "You were disconnected from WhatsApp",
            reconnect: "Reconnect (scan QR)",
            pair: "Connect with a phone number",
            later: "Not now",
            use_here: "Use here",
            readonly: "Disconnected — read only",
            expired: "The code expired",
            new_code: "Generate a new code",
            other_title: "This is a different account",
            other_body: "This is a different account. Delete the previous account's history?",
            wipe: "Delete history",
            keep: "Keep history",
            offline: "No connection. Trying again…",
            offline_long: "No connection for",
            try_now: "Try now",
        },
    }
}

pub fn reason_line(locale: &str, kind: EndKind) -> &'static str {
    match (locale, kind) {
        ("pt", EndKind::Removed) => "Este aparelho foi removido pelo celular.",
        ("pt", EndKind::Replaced) => "Esta sessão foi aberta em outro lugar.",
        ("pt", EndKind::Banned) => "A conta foi bloqueada temporariamente.",
        ("pt", EndKind::Expired) => "O código de conexão expirou.",
        ("pt", EndKind::Unknown) => "O WhatsApp encerrou a sessão sem dizer o motivo.",
        ("es", EndKind::Removed) => "Este dispositivo fue quitado desde el celular.",
        ("es", EndKind::Replaced) => "Esta sesión se abrió en otro lugar.",
        ("es", EndKind::Banned) => "La cuenta fue bloqueada temporalmente.",
        ("es", EndKind::Expired) => "El código de conexión expiró.",
        ("es", EndKind::Unknown) => "WhatsApp cerró la sesión sin decir el motivo.",
        (_, EndKind::Removed) => "This device was removed from the phone.",
        (_, EndKind::Replaced) => "This session was opened somewhere else.",
        (_, EndKind::Banned) => "The account was blocked for a while.",
        (_, EndKind::Expired) => "The connection code expired.",
        (_, EndKind::Unknown) => "WhatsApp ended the session without a reason.",
    }
}

pub fn notify_lines(locale: &str, kind: EndKind, clock: &str) -> (String, String) {
    let words = notice(locale);
    (
        words.title.to_owned(),
        format!("{} {clock}", reason_line(locale, kind)),
    )
}

/// Banner while the network is down. After two minutes it names the wait.
pub fn offline_label(locale: &str, elapsed: Duration, reason: &str) -> String {
    let words = notice(locale);
    if elapsed < OFFLINE_NOTICE {
        return format!("{} ({reason})", words.offline);
    }
    let minutes = elapsed.as_secs() / 60;
    format!("{} {minutes} min ({reason})", words.offline_long)
}

/// How long to wait before fetching one sticker again.
/// A rate limit holds the whole batch for at least a minute.
pub fn sticker_wait(attempt: u32, rate_limited: bool) -> Duration {
    let step = attempt.min(4);
    let seconds = if rate_limited { 60u64 } else { 15u64 };
    Duration::from_secs(seconds.saturating_mul(1u64 << step))
}

pub fn rate_limited(error: &str) -> bool {
    error.contains("rate-overlimit") || error.contains("429")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logout_choices_keep_history_until_the_account_changes() {
        assert!(same_account(None, None, "1@s.whatsapp.net", "1@lid"));
        assert!(same_account(
            Some("1@s.whatsapp.net"),
            Some("9@lid"),
            "1@s.whatsapp.net",
            "9@lid"
        ));
        assert!(!same_account(
            Some("1@s.whatsapp.net"),
            Some("9@lid"),
            "2@s.whatsapp.net",
            "8@lid"
        ));
    }

    #[test]
    fn connecting_without_a_session_becomes_disconnected() {
        assert!(!connecting_gave_up(Duration::from_secs(89), false));
        assert!(connecting_gave_up(CONNECTING_LIMIT, false));
        assert!(!connecting_gave_up(Duration::from_secs(600), true));
        assert!(!status_may_leave_hold(HoldNext::Connecting));
        assert!(!status_may_leave_hold(HoldNext::Disconnected));
        assert!(status_may_leave_hold(HoldNext::Unlinked));
        assert!(!replaced_reconnects());
        assert!(!accept_connected(true));
        assert!(accept_connected(false));
        assert!(!session_on_disk(0));
        assert!(session_on_disk(1));
    }

    #[test]
    fn a_link_log_never_contains_a_qr_payload() {
        let secret = "2@fixture-secret-not-a-real-code";
        let line = link_log(LinkLog::Qr { index: 3 });
        assert_eq!(line, "QR gerado (3/6)");
        assert!(!line.contains(secret));
        assert!(!line.contains("qr: Some"));
        let ended = link_log(LinkLog::Ended(EndKind::Removed));
        assert!(!ended.contains(secret));
        assert!(decode_end(&encode_end(EndKind::Replaced, 10)) == Some((EndKind::Replaced, 10)));
    }

    #[test]
    fn offline_copy_changes_after_two_minutes() {
        let short = offline_label("pt", Duration::from_secs(30), "reset");
        assert!(short.contains("Tentando"));
        assert!(!short.contains("há"));
        let long = offline_label("pt", Duration::from_secs(125), "reset");
        assert!(long.contains("2 min"));
    }

    #[test]
    fn a_rate_limit_waits_at_least_a_minute() {
        assert!(sticker_wait(0, true) >= Duration::from_secs(60));
        assert!(sticker_wait(1, true) > sticker_wait(0, true));
        assert!(sticker_wait(0, false) < sticker_wait(0, true));
        assert!(rate_limited("rate-overlimit"));
        assert!(rate_limited("status 429"));
        assert!(!rate_limited("not found"));
        let pt = notice("pt");
        assert_eq!(pt.later, "Agora não");
        assert_eq!(
            reason_line("es", EndKind::Replaced),
            "Esta sesión se abrió en otro lugar."
        );
    }
}
