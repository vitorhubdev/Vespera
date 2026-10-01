//! Windows toast identity and compact sender avatars, including portable builds.

use std::{path::Path, sync::OnceLock};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegCloseKey, RegCreateKeyW, RegSetValueExW,
};
use winrt_notification::{IconCrop, Toast};

const APPLICATION_ID: &str = "io.github.vitorhubdev.Vespera";

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn register_identity() -> std::io::Result<()> {
    let path = wide(&format!(
        r"Software\Classes\AppUserModelId\{APPLICATION_ID}"
    ));
    let name = wide("DisplayName");
    let value = wide("Vespera");
    let mut key = std::ptr::null_mut();
    // All buffers are NUL-terminated UTF-16 and remain alive during each call.
    let status = unsafe { RegCreateKeyW(HKEY_CURRENT_USER, path.as_ptr(), &mut key) };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    let status = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_SZ,
            value.as_ptr().cast(),
            (value.len() * size_of::<u16>()) as u32,
        )
    };
    // Close the key even when writing its display name failed.
    unsafe { RegCloseKey(key) };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

fn notification(title: &str, body: &str, picture: Option<&Path>) -> Toast {
    let toast = Toast::new(APPLICATION_ID).title(title).text1(body);
    if let Some(picture) = picture {
        toast.icon(picture, IconCrop::Circular, "Sender")
    } else {
        toast
    }
}

pub(super) fn show(
    title: &str,
    body: &str,
    picture: Option<&Path>,
    mut activated: impl FnMut() + Send + 'static,
) -> anyhow::Result<()> {
    register()?;
    notification(title, body, picture)
        .on_activated(move |_| {
            activated();
            Ok(())
        })
        .show()?;
    Ok(())
}

/// A message toast with a reply field and a mark-as-read button.
/// The callback runs on the notification thread and must not log the reply.
pub(super) fn show_message(
    title: &str,
    body: &str,
    picture: Option<&Path>,
    mut chosen: impl FnMut(super::ToastChoice) + Send + 'static,
) -> anyhow::Result<()> {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::core::{HSTRING, Ref};

    register()?;
    let picture = picture.map(|path| path.display().to_string());
    let markup = super::message_xml(title, body, picture.as_deref());
    let xml = XmlDocument::new()?;
    xml.LoadXml(&HSTRING::from(markup))?;
    let toast = ToastNotification::CreateToastNotification(&xml)?;
    toast.Activated(&TypedEventHandler::<
        ToastNotification,
        windows::core::IInspectable,
    >::new(
        move |_sender: Ref<'_, ToastNotification>,
              inspect: Ref<'_, windows::core::IInspectable>| {
            let (argument, reply) = activation_text(inspect.as_ref());
            chosen(super::toast_choice(argument.as_deref(), &reply));
            Ok(())
        },
    ))?;
    let notifier =
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APPLICATION_ID))?;
    notifier.Show(&toast)?;
    Ok(())
}

fn activation_text(inspect: Option<&windows::core::IInspectable>) -> (Option<String>, String) {
    use windows::Foundation::IPropertyValue;
    use windows::UI::Notifications::ToastActivatedEventArgs;
    use windows::core::{HSTRING, Interface};

    let Some(inspect) = inspect else {
        return (None, String::new());
    };
    let Ok(args) = inspect.cast::<ToastActivatedEventArgs>() else {
        return (None, String::new());
    };
    let argument = args
        .Arguments()
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    let reply = args
        .UserInput()
        .ok()
        .and_then(|input| input.Lookup(&HSTRING::from("reply")).ok())
        .and_then(|value| value.cast::<IPropertyValue>().ok())
        .and_then(|property| property.GetString().ok())
        .map(|value| value.to_string())
        .unwrap_or_default();
    (argument, reply)
}

fn register() -> anyhow::Result<()> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    if let Err(error) = REGISTERED.get_or_init(|| register_identity().map_err(|e| e.to_string())) {
        anyhow::bail!("notification identity unavailable: {error}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_shortcuts_use_the_toast_identity() {
        let installer = include_str!("../../packaging/windows/vespera.iss");
        let shortcuts: Vec<_> = installer
            .lines()
            .filter(|line| {
                line.starts_with("Name: \"{autoprograms}")
                    || line.starts_with("Name: \"{autodesktop}")
            })
            .collect();
        assert_eq!(shortcuts.len(), 2);
        for shortcut in shortcuts {
            assert!(shortcut.contains(&format!("AppUserModelID: \"{APPLICATION_ID}\"")));
        }
    }
}
