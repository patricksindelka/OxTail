//! What the GUI remembers between runs beyond the session file: column
//! choices and layouts per file, the split layout and merged tabs
//! (`gui-state.json`), against a real data folder in a temporary directory.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{logfmt_sample, step_until};
use egui::vec2;
use egui_kittest::Harness;
use oxtail_config::{DataDir, ResolveInput};
use oxtail_gui::guistate::{GUI_STATE_FILE, GuiState};
use oxtail_gui::{AppInit, ExternalOpen, OpenRequest, OxTailApp, load_startup};

fn data_dir(root: &Path) -> DataDir {
    DataDir::resolve(ResolveInput {
        cli_override: Some(root.to_path_buf()),
        exe_path: PathBuf::new(),
        installed_dir: None,
    })
}

fn harness_in(root: &Path, request: OpenRequest) -> Harness<'static, OxTailApp> {
    Harness::builder()
        .with_size(vec2(1000.0, 500.0))
        .build_ui_state(
            |ui, app: &mut OxTailApp| app.show(ui),
            OxTailApp::from_init(AppInit {
                startup: load_startup(data_dir(root)),
                request,
                external: ExternalOpen::disconnected(),
            }),
        )
}

fn write_log(dir: &Path, name: &str, lines: usize) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, logfmt_sample(lines)).unwrap();
    p
}

#[test]
fn column_choices_and_layouts_survive_a_restart() {
    let data = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let path = write_log(logs.path(), "app.log", 200);

    // First run: accept the suggestion, hide a column, resize another.
    let mut h = harness_in(data.path(), OpenRequest::file(&path));
    step_until(&mut h, "a suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    h.state_mut().active_view_mut().unwrap().accept_suggestion();
    step_until(&mut h, "the table", |a| {
        a.active_view().is_some_and(|v| v.table_active())
    });
    let (hidden_name, wide_name) = {
        let v = h.state_mut().active_view_mut().unwrap();
        let schema = v.st.parser.as_ref().unwrap().schema().clone();
        let took = v.st.layout.position(schema.find("took").unwrap()).unwrap();
        let msg = v.st.layout.position(schema.find("msg").unwrap()).unwrap();
        v.st.layout.set_visible(took, false);
        v.st.layout.resize(msg, 77.0);
        v.st.dirty = true;
        ("took".to_string(), "msg".to_string())
    };
    h.step();
    h.state_mut().shutdown();
    let saved = GuiState::parse(&std::fs::read(data.path().join(GUI_STATE_FILE)).unwrap());
    let entry = saved.structure(&path).expect("the file is remembered");
    assert!(entry.table && entry.spec.is_some());
    let layout = entry.layout.as_ref().unwrap();
    assert_eq!(layout.hidden, vec![hidden_name.clone()]);
    assert_eq!(layout.widths[&wide_name], 77.0);
    drop(h);

    // Second run: the table comes back as it was, without asking again.
    let mut h = harness_in(data.path(), OpenRequest::default());
    step_until(&mut h, "the restored table", |a| {
        a.active_view()
            .is_some_and(|v| v.table_active() && !v.last_rows.is_empty())
    });
    let v = h.state().active_view().unwrap();
    assert!(v.st.suggestion.is_none());
    let schema = v.st.parser.as_ref().unwrap().schema();
    let took = v.st.layout.position(schema.find("took").unwrap()).unwrap();
    let msg = v.st.layout.position(schema.find("msg").unwrap()).unwrap();
    assert!(!v.st.layout.cols[took].visible);
    assert_eq!(v.st.layout.cols[msg].width, 77.0);
}

#[test]
fn a_dismissed_suggestion_is_not_shown_again() {
    let data = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let path = write_log(logs.path(), "app.log", 100);
    let mut h = harness_in(data.path(), OpenRequest::file(&path));
    step_until(&mut h, "a suggestion", |a| {
        a.active_view().is_some_and(|v| v.st.suggestion.is_some())
    });
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .dismiss_suggestion();
    h.step();
    h.state_mut().shutdown();
    drop(h);
    let mut h = harness_in(data.path(), OpenRequest::default());
    step_until(&mut h, "the tab", |a| {
        a.active_view().is_some_and(|v| v.st.decided)
    });
    let v = h.state().active_view().unwrap();
    assert!(v.st.suggestion.is_none() && !v.table_active());
}

#[test]
fn panes_and_merged_tabs_are_restored() {
    let data = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let a = write_log(logs.path(), "a.log", 60);
    let b = write_log(logs.path(), "b.log", 60);

    let mut h = harness_in(data.path(), OpenRequest::default());
    h.step();
    h.state_mut().open_request(OpenRequest::file(&a));
    h.state_mut().open_request(OpenRequest::file(&b));
    step_until(&mut h, "two tabs", |x| {
        x.tab_list().len() == 2 && x.tab_list().iter().all(|t| t.view().is_some())
    });
    // Split: the active tab (b) is shown again in a new pane to the right.
    h.state_mut()
        .split_active(oxtail_config::SplitDirection::Horizontal);
    // And a merged tab of both files.
    h.state_mut().open_merged_paths(vec![a.clone(), b.clone()]);
    step_until(&mut h, "three plain tabs and a merged one", |x| {
        x.tab_list().len() == 4
            && x.tab_list().iter().filter(|t| t.view().is_some()).count() == 3
            && x.tab_list().iter().any(|t| t.merged().is_some())
    });
    assert_eq!(h.state().pane_count(), 2);
    h.state_mut().shutdown();
    let saved = GuiState::parse(&std::fs::read(data.path().join(GUI_STATE_FILE)).unwrap());
    let panes = saved.panes.expect("the layout is saved");
    assert_eq!(panes.tab_panes.len(), 3);
    assert_eq!(saved.merged.len(), 1);
    assert_eq!(saved.merged[0].paths, vec![a.clone(), b.clone()]);
    drop(h);

    // The next run: same panes, same tabs in them, the merged tab is back.
    let mut h = harness_in(data.path(), OpenRequest::default());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        h.step();
        let app = h.state();
        if app.tab_list().len() == 4
            && app
                .tab_list()
                .iter()
                .any(|t| t.merged().is_some_and(|m| m.len() == 120))
            && app.tab_list().iter().filter(|t| t.view().is_some()).count() == 3
        {
            break;
        }
        assert!(Instant::now() < deadline, "the state did not come back");
        std::thread::sleep(Duration::from_millis(2));
    }
    let app = h.state();
    assert_eq!(app.pane_count(), 2);
    let plain: Vec<_> = app.tab_list().iter().filter(|t| t.path.is_some()).collect();
    let panes_of: Vec<u32> = plain.iter().map(|t| t.pane).collect();
    assert_eq!(panes_of, panes.tab_panes);
}
