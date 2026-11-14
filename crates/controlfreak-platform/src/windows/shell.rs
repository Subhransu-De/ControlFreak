//! Identifies the Windows shell's own windows: the taskbar, the desktop and the Start and Search
//! surfaces.
//!
//! `IVirtualDesktopManager::GetWindowDesktopId` fails with `TYPE_E_ELEMENTNOTFOUND` for these
//! windows because they are present on every virtual desktop. Target references need a desktop
//! identity, so these windows are recognized here and treated as present on every desktop.
//!
//! A window class is not an identity: any process can register a window with the class name
//! `Shell_TrayWnd`. A window is a shell surface only if its class is one the shell creates and
//! the process that owns it runs the matching image from the Windows directory. Both facts are
//! needed, and every other target integrity check still applies to the window afterwards.

/// Desktop identity recorded for shell surfaces, which belong to every virtual desktop.
pub(super) const ALL_DESKTOPS: &str = "shell:all-desktops";

const EXPLORER: &str = "explorer.exe";
const APP_FRAME_HOST: [&str; 2] = ["system32", "applicationframehost.exe"];
const EXPLORER_CLASSES: [&str; 4] = [
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
];
/// Class of the content window a packaged app hosts inside its frame.
pub(super) const CORE_WINDOW_CLASS: &str = "Windows.UI.Core.CoreWindow";
/// Start and Search hosts: executable name and the prefix of the package folder that holds it
/// under `SystemApps`.
const SYSTEM_APP_HOSTS: [(&str, &str); 3] = [
    (
        "startmenuexperiencehost.exe",
        "microsoft.windows.startmenuexperiencehost_",
    ),
    ("searchhost.exe", "microsoftwindows.client.cbs_"),
    ("searchapp.exe", "microsoft.windows.search_"),
];

/// Lowercases a Win32 path, unifies separators and rejects anything that is not a plain
/// absolute path: relative components, empty components, or device prefixes other than the
/// extended-length one.
fn components(path: &str) -> Option<Vec<String>> {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let path = path.replace('/', r"\").to_lowercase();
    let parts: Vec<String> = path.split('\\').map(str::to_owned).collect();
    let drive = parts.first()?;
    let has_drive = drive.len() == 2
        && drive.ends_with(':')
        && drive.chars().next().is_some_and(|c| c.is_ascii_lowercase());
    // Allow one trailing separator, as in a directory path, but no other empty component.
    let trimmed: &[String] = match parts.last() {
        Some(last) if last.is_empty() => &parts[..parts.len() - 1],
        _ => &parts,
    };
    (has_drive
        && trimmed.len() >= 2
        && trimmed
            .iter()
            .all(|part| !part.is_empty() && part != "." && part != ".."))
    .then(|| trimmed.to_vec())
}

/// The components of `image` below `windows_directory`, or `None` if it is not inside it.
fn below_windows(image: &str, windows_directory: &str) -> Option<Vec<String>> {
    let root = components(windows_directory)?;
    let image = components(image)?;
    (image.len() > root.len() && image[..root.len()] == root[..])
        .then(|| image[root.len()..].to_vec())
}

/// Whether `image` is the Windows shell host, `explorer.exe` in the Windows directory itself.
fn is_shell_host_image(image: &str, windows_directory: &str) -> bool {
    below_windows(image, windows_directory).is_some_and(|rest| rest == [EXPLORER])
}

/// Whether `image` runs a host that owns windows of many unrelated applications: the shell
/// (`explorer.exe`) or the frame host of packaged apps (`System32\ApplicationFrameHost.exe`).
/// For these, a shared process does not make two windows one application.
pub(super) fn is_shared_host_image(image: &str, windows_directory: &str) -> bool {
    is_shell_host_image(image, windows_directory)
        || is_app_frame_host_image(image, windows_directory)
}

/// Whether `image` is `System32\ApplicationFrameHost.exe`, which owns the frame window of every
/// packaged (UWP) app while the app's own process owns the content window hosted inside it.
pub(super) fn is_app_frame_host_image(image: &str, windows_directory: &str) -> bool {
    below_windows(image, windows_directory).is_some_and(|rest| rest == APP_FRAME_HOST)
}

/// Whether a window with this class, owned by a process running `image`, is a shell surface.
pub(super) fn is_shell_surface(class: &str, image: &str, windows_directory: &str) -> bool {
    if EXPLORER_CLASSES.contains(&class) {
        return is_shell_host_image(image, windows_directory);
    }
    if class != CORE_WINDOW_CLASS {
        return false;
    }
    let Some(rest) = below_windows(image, windows_directory) else {
        return false;
    };
    // SystemApps\<package folder>\<executable>, with nothing deeper.
    let [folder, package, executable] = rest.as_slice() else {
        return false;
    };
    folder == "systemapps"
        && SYSTEM_APP_HOSTS
            .iter()
            .any(|(name, prefix)| executable == name && package.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOWS: &str = r"C:\Windows";

    #[test]
    fn explorer_classes_need_the_explorer_image_from_the_windows_directory() {
        for class in EXPLORER_CLASSES {
            assert!(is_shell_surface(class, r"C:\Windows\explorer.exe", WINDOWS));
            assert!(is_shell_surface(class, r"c:\WINDOWS\Explorer.EXE", WINDOWS));
            assert!(is_shell_surface(
                class,
                r"\\?\C:\Windows\explorer.exe",
                WINDOWS
            ));
            assert!(is_shell_surface(class, "C:/Windows/explorer.exe", WINDOWS));
        }
    }

    #[test]
    fn shared_hosts_are_the_shell_and_the_packaged_app_frame_host_from_the_windows_directory() {
        for image in [
            r"C:\Windows\explorer.exe",
            r"c:\windows\system32\ApplicationFrameHost.exe",
        ] {
            assert!(is_shared_host_image(image, WINDOWS), "{image}");
        }
        for image in [
            r"C:\Windows\notepad.exe",
            r"C:\Windows\ApplicationFrameHost.exe",
            r"C:\Users\someone\System32\ApplicationFrameHost.exe",
            r"C:\Windows\System32\ApplicationFrameHost.exe.exe",
            "",
        ] {
            assert!(!is_shared_host_image(image, WINDOWS), "{image}");
        }
        // The frame host is not a shell surface: only explorer.exe gets the desktop exemption.
        assert!(!is_shell_surface(
            "Shell_TrayWnd",
            r"C:\Windows\System32\ApplicationFrameHost.exe",
            WINDOWS
        ));
    }

    #[test]
    fn a_shell_class_alone_is_never_enough() {
        for image in [
            r"C:\Users\someone\Downloads\explorer.exe",
            r"C:\Windows\Temp\explorer.exe",
            r"C:\Windows\System32\explorer.exe",
            r"C:\Windows\explorer.exe.exe",
            r"C:\Windows\notepad.exe",
            r"D:\Windows\explorer.exe",
            r"C:\WindowsEvil\explorer.exe",
            r"C:\Windows\..\Users\x\explorer.exe",
            r"C:\Windows\.\explorer.exe",
            r"explorer.exe",
            r"\Windows\explorer.exe",
            r"\\server\share\Windows\explorer.exe",
            "",
        ] {
            for class in EXPLORER_CLASSES {
                assert!(!is_shell_surface(class, image, WINDOWS), "{image}");
            }
            assert!(!is_shell_host_image(image, WINDOWS), "{image}");
        }
        // An unknown class never qualifies, even for the real shell process.
        for class in ["", "CabinetWClass", "Shell_Fake", "Notepad", "#32768"] {
            assert!(!is_shell_surface(
                class,
                r"C:\Windows\explorer.exe",
                WINDOWS
            ));
        }
    }

    #[test]
    fn start_and_search_hosts_must_live_in_their_system_app_package() {
        let start = r"C:\Windows\SystemApps\Microsoft.Windows.StartMenuExperienceHost_cw5n1h2txyewy\StartMenuExperienceHost.exe";
        let search =
            r"C:\Windows\SystemApps\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\SearchHost.exe";
        let old_search =
            r"C:\Windows\SystemApps\Microsoft.Windows.Search_cw5n1h2txyewy\SearchApp.exe";
        for image in [start, search, old_search] {
            assert!(
                is_shell_surface(CORE_WINDOW_CLASS, image, WINDOWS),
                "{image}"
            );
            // The class that is not a core window does not qualify these processes.
            assert!(
                !is_shell_surface("Shell_TrayWnd", image, WINDOWS),
                "{image}"
            );
        }
        for image in [
            // Right name, wrong place.
            r"C:\Users\x\StartMenuExperienceHost.exe",
            r"C:\Windows\StartMenuExperienceHost.exe",
            r"C:\Windows\System32\StartMenuExperienceHost.exe",
            r"C:\Windows\SystemApps\StartMenuExperienceHost.exe",
            r"C:\Windows\SystemApps\Other.App_1\StartMenuExperienceHost.exe",
            r"C:\Windows\SystemApps\Microsoft.Windows.StartMenuExperienceHost_x\sub\StartMenuExperienceHost.exe",
            r"C:\Windows\SystemApps\Microsoft.Windows.StartMenuExperienceHost_x\..\Other\StartMenuExperienceHost.exe",
            r"C:\Windows\WinSxS\Microsoft.Windows.StartMenuExperienceHost_x\StartMenuExperienceHost.exe",
            // Right place, wrong program, and another host's folder for the executable.
            r"C:\Windows\SystemApps\Microsoft.Windows.StartMenuExperienceHost_x\cmd.exe",
            r"C:\Windows\SystemApps\MicrosoftWindows.Client.CBS_x\StartMenuExperienceHost.exe",
            r"C:\Windows\SystemApps\Microsoft.Windows.StartMenuExperienceHost_x\SearchHost.exe",
            // TextInputHost shares the class and is deliberately not a shell target.
            r"C:\Windows\SystemApps\MicrosoftWindows.Client.CBS_x\TextInputHost.exe",
            "",
        ] {
            assert!(
                !is_shell_surface(CORE_WINDOW_CLASS, image, WINDOWS),
                "{image}"
            );
        }
    }

    #[test]
    fn a_custom_windows_directory_is_honoured_and_a_bad_one_matches_nothing() {
        let root = r"D:\Win";
        assert!(is_shell_surface("Progman", r"D:\Win\explorer.exe", root));
        assert!(!is_shell_surface(
            "Progman",
            r"C:\Windows\explorer.exe",
            root
        ));
        assert!(is_shell_host_image(r"D:\Win\explorer.exe", r"D:\Win\"));
        for bad in ["", "Windows", r"C:\", r"C:\Windows\..", r"\\?\"] {
            assert!(
                !is_shell_surface("Progman", r"C:\Windows\explorer.exe", bad),
                "{bad}"
            );
        }
    }

    #[test]
    fn only_the_system32_frame_host_is_the_app_frame_host() {
        assert!(is_app_frame_host_image(
            r"C:\Windows\System32\ApplicationFrameHost.exe",
            WINDOWS
        ));
        for image in [
            r"C:\Windows\explorer.exe",
            r"C:\Windows\ApplicationFrameHost.exe",
            r"C:\Users\someone\System32\ApplicationFrameHost.exe",
            r"C:\Windows\System32\ApplicationFrameHost.exe.exe",
        ] {
            assert!(!is_app_frame_host_image(image, WINDOWS), "{image}");
        }
    }

    #[test]
    fn only_the_windows_directory_explorer_is_the_shell_host() {
        assert!(is_shell_host_image(r"C:\Windows\explorer.exe", WINDOWS));
        assert!(!is_shell_host_image(
            r"C:\Windows\explorer.exe.bak",
            WINDOWS
        ));
        assert!(!is_shell_host_image(
            r"C:\Windows\SystemApps\X_1\explorer.exe",
            WINDOWS
        ));
    }
}
