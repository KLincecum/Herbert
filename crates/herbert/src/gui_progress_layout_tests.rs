//! Render synthetic status only; no preferences, profiles, polling or connections.
use super::*;
fn fixture(active: bool) -> Value {
    let keys = json!({"Key1":"Ice.UIDbd.ExampleProductionDetail","Key2":format!("Ice.UIDbd.ExampleProductionDetail\\{}\\layout.jsonc", "LongNativeIdentifier".repeat(3)), "TypeCode":"KNTCMetaUI"});
    json!({
        "phase":if active {"running"} else {"partial"}, "executionActive":active,
        "categories":["A","B"],"categoryNames":{"A":"Kinetic applications and layers","B":"Report styles and layouts"},
        "discoveryFinished":{"A":true,"B":false},"discovered":{"A":[{},{}],"B":[{}]},
        "fileCount":17309,"fileBytes":811178393,
        "statistics":{"elapsedMs":3661000,"requests":160,"requestMs":12500,
            "receivedBytes":191111152,"pages":30,"pageWriteMs":791590,
            "pluginElapsedMs":{"A":3661000}},
        "activityStages":["Helper: Installing temporary helper in Epicor","Host helper: Packaging files on the server","Host helper: Downloading and verifying server package"],
        "activeOperations":(0..8).map(|n|json!({"plugin":"Kinetic applications and layers",
            "identity":{"keys":keys},"step":"Preparing download","elapsedSeconds":n})).collect::<Vec<_>>(),
        "artifacts":[{"identity":{"category":"A","kind":"PersistedKineticApplicationFile","keys":keys},
            "acquisition":"collected","processing":"derived"}],
        "diagnostics":(0..3).map(|_|json!({"message":"A synthetic diagnostic with enough text to wrap at the minimum supported window width. The full explanation must remain available."})).collect::<Vec<_>>(),
        "acquisitions":[{"path":format!("Herbert Data/raw/{}/fixture.txt","a".repeat(64))}]
    })
}
fn icon(ctx: &egui::Context) -> egui::TextureHandle {
    let icon=eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/branding/herbert-vacuum.png")).unwrap();
    ctx.load_texture("layout-vacuum",egui::ColorImage::from_rgba_unmultiplied([icon.width as usize,icon.height as usize],&icon.rgba),egui::TextureOptions::LINEAR)
}

pub(super) fn app(ctx: &egui::Context, runtime: &tokio::runtime::Runtime, active: bool) -> Gui {
    let root = std::env::temp_dir().join(format!("herbert-layout-{}", std::process::id()));
    let (sender, receiver) = mpsc::channel();
    let brand_icon = icon(ctx);
            let app = Gui {
                collection_method: Default::default(),
                host: Host::with_roots(root.join("state"), root.join("plugins"), Some(root.join("coordination"))),
                runtime: runtime.handle().clone(),
                sender,
                receiver,
                tab: Tab::Download,
                about: Default::default(),
                brand_icon,
                settings: SettingsForm::new(&Default::default()),
                settings_loaded: false,
                capabilities_loaded: false,
                working: false,
                picker_pending: false,
                diagnostic_export_pending: false,
                generation: 0,
                notice: String::new(),
                diagnostic: None,
                error_detail: None,
                profiles: vec![],
                profile_id: String::new(),
                name: String::new(),
                url: String::new(),
                company: String::new(),
                username: String::new(),
                password: String::new(),
                api_key: String::new(),
                mode: AuthMode::Session,
                source: Source::Entered,
                credential_status: None,
                input_file: String::new(),
                file_environment: String::new(),
                file_choices: vec![],
                file_choices_path: String::new(),
                file_company_override: false,
                file_help_open: false,
                preferences: Default::default(),
                completion_sound: Default::default(),
                preferences_ready: false,
                profiles_loaded: false,
                preferences_bytes: vec![],
                preferences_error: None,
                capabilities: vec![],
                categories: BTreeMap::new(),
                plugin_settings: BTreeMap::new(),
                destination: String::new(),
                output_mode: Some(true),
                temporary_access: crate::application::default_temporary_access(),
                account_readiness: None,
                allow_untrusted_certificate: false,
                company_choices: None,
                testing_connection: false,
                selected_authorization: None,
                connected_key: None,
                pending_download: None,
                authorization_choice: Default::default(),
                authorization_workflow: String::new(),
                saved_authorizations: Default::default(),
                connected: false,
                cleanup_pending: false,
                run_id: "synthetic-layout".into(),
                output_root: r"C:\Synthetic output\A deliberately long destination folder for layout verification".into(),
                folder_open_error: None,
                journal: Some(fixture(active)),
                poll_pending: false,
                last_poll: Instant::now(),
                launch_stage: LaunchStage::Requested,
            };

    app
}
#[test]
fn launch_requests_capabilities_only_after_first_visible_frame() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let ctx = egui::Context::default();
    let mut app = app(&ctx, &runtime, false);
    app.launch_stage = LaunchStage::FirstFrame;
    // eframe shows the window after painting this frame; nothing may load during it.
    assert!(app.launch_requests(&ctx).is_empty());
    assert_eq!(app.launch_requests(&ctx), ["profiles.list", "settings.inspect", "capabilities"]);
    assert!(app.launch_requests(&ctx).is_empty());
}
fn panel(ui: &mut egui::Ui, app: &mut Gui) -> (egui::Vec2, egui::Rect) {
    app.navigation(ui);
    egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(20)).show(ui, |ui| {
        let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            app.download_content(ui);
            ui.add_space(12.0);
        });
        (scroll.content_size, scroll.inner_rect)
    }).inner
}
#[test]
fn progress_wraps_long_paths_at_default_and_narrow_sizes() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for (width,height) in [(720.0,560.0),(1180.0,820.0),(1360.0,900.0)] {
        for (active, with_batches) in [(true, false), (false, false), (false, true)] {
            let ctx = egui::Context::default();
            configure_style(&ctx);
            let mut app = app(&ctx, &runtime, active);
            if with_batches {
                app.journal.as_mut().unwrap()["transferBatches"] = json!(
                    (0..40).map(|_| json!({"plugin":"BAQs", "files":24, "processed":24,
                        "phase":"Processed"})).collect::<Vec<_>>());
            }
            let mut geometry = None;
            for _ in 0..3 {
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,height))),
                    ..Default::default()
                }, |ui| { geometry = Some(panel(ui, &mut app)); });
                output.textures_delta.clear();
            }
            let (content, viewport) = geometry.unwrap();
            assert!(content.x <= viewport.width()+1.0, "horizontal overflow: {content:?} in {viewport:?}");
            if width >= 1180.0 {
                assert!(content.y <= viewport.height(), "default hides status panels: {content:?} in {viewport:?}");
            }
            println!("{width}x{height} active={active}: content={content:?} viewport={:?}", viewport.size());
        }
    }
}

#[test]
fn full_download_setup_fits_with_collection_method_and_sound() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for (width, height) in [(1180.0, 820.0), (1360.0, 900.0)] {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let mut app = app(&ctx, &runtime, false);
        app.run_id.clear();
        let mut geometry = None;
        for _ in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height))),
                ..Default::default()
            }, |ui| { geometry = Some(panel(ui, &mut app)); });
            output.textures_delta.clear();
        }
        let (content, viewport) = geometry.unwrap();
        assert!(content.y <= viewport.height(), "Setup requires scrolling: {content:?} in {viewport:?}");
        assert!(content.x <= viewport.width() + 1.0);
    }
}

#[test]
fn eight_running_plugins_are_visible_together_above_queued_work() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for width in [1180.0, 1360.0] {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let mut app = app(&ctx, &runtime, true);
        let ids = (0..12).map(|n|format!("P{n}")).collect::<Vec<_>>();
        let names = ids.iter().enumerate().map(|(n,id)|(id.clone(),json!(format!("Plugin {n}")))).collect::<serde_json::Map<_,_>>();
        let states = ids.iter().enumerate().filter(|(n,_)|*n>=4).map(|(_,id)|(id.clone(),json!("running"))).collect::<serde_json::Map<_,_>>();
        app.journal=Some(json!({"executionActive":true,"phase":"acquiring","categories":ids,
            "categoryNames":names,"pluginStates":states,
            "activeOperations":(4..12).map(|n|json!({"plugin":format!("Plugin {n}"),"step":format!("Working on page {n}"),"identity":null})).collect::<Vec<_>>()}));
        for _ in 0..3 {
            let mut output=ctx.run_ui(egui::RawInput {screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,820.0))),..Default::default()},|ui|{panel(ui,&mut app);});
            for n in 4..12 {
                assert!(output.shapes.iter().any(|shape|match &shape.shape {
                    egui::Shape::Text(text) => text.galley.text()==format!("Plugin {n}")
                        && shape.clip_rect.expand(1.0).contains_rect(egui::Rect::from_min_size(text.pos,text.galley.size())),
                    _=>false,
                }),"Running plugin {n} is not visible at {width}");
            }
            output.textures_delta.clear();
        }
    }
}

#[test]
fn current_stage_stays_visible_above_completed_batch_history() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for step in ["Uploading and importing helper package", "Server exports: 120/365 ready; 96 files downloaded"] {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let mut app = app(&ctx, &runtime, true);
        app.journal = Some(json!({"phase":"acquiring", "executionActive":true,
            "activeOperations":[{"plugin":"BAQs","identity":null,"step":step,"elapsedSeconds":20}],
            "transferBatches":(0..40).map(|_|json!({"plugin":"BAQs","files":24,"verified":24,"phase":"Verified"})).collect::<Vec<_>>() }));
        for _ in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1180.0,820.0))),
                ..Default::default()
            }, |ui| { panel(ui, &mut app); });
            output.textures_delta.clear();
            assert!(output.shapes.iter().any(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    text.galley.text().contains(step) && text.pos.y < 250.0
                        && shape.clip_rect.contains(text.pos)
                } else { false }
            }), "Current stage is hidden by batch history: {step}");
        }
    }
}

#[test]
fn status_changes_do_not_move_controls_or_progress_track() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for width in [720.0, 1180.0, 1360.0] {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let mut app = app(&ctx, &runtime, true);
        let mut baseline = None;
        for count in [0, 1, 8, 40, 0] {
            app.journal = Some(json!({"phase":"acquiring", "executionActive":true, "pauseRequested": count == 8,
                "activeOperations":(0..count).map(|_|json!({"plugin":"BAQs", "identity":null,
                    "step":"A changing helper status with enough text to wrap across several lines. ".repeat(4)})).collect::<Vec<_>>(),
                "transferBatches":(0..count).map(|_|json!({"plugin":"BAQs","files":24,"verified":24,"phase":"Processed","processed":24})).collect::<Vec<_>>() }));
            let mut positions = Vec::new();
            for _ in 0..3 {
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,900.0))),
                    ..Default::default()
                }, |ui| { panel(ui, &mut app); });
                positions = output.shapes.iter().filter_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape {
                        let label = text.galley.text();
                        if label.starts_with("All plugins:") || label.starts_with("Work:") {
                            let bounds = egui::Rect::from_min_size(text.pos, text.galley.size());
                            assert!(shape.clip_rect.expand(1.0).contains_rect(bounds),
                                "Progress label is clipped at width {width}: {bounds:?} in {:?}", shape.clip_rect);
                        }
                        if label == "Cancel" || label == "Open download folder" || label.starts_with("All plugins:") {
                            return Some((if label.starts_with("All plugins:") { "All plugins:".to_owned() } else { label.to_string() }, if label.starts_with("All plugins:") { 0.0 } else { text.pos.x }, text.pos.y));
                        }
                    }
                    None
                }).collect();
                output.textures_delta.clear();
            }
            assert_eq!(positions.len(), 3, "The controls and progress must remain visible");
            if let Some(expected) = &baseline { assert_eq!(&positions, expected, "Layout moved at {width} with {count} status entries"); }
            else { baseline = Some(positions); }
        }
    }
}

#[test]
#[ignore = "Render a supplied status snapshot; no connection or personal state is loaded."]
fn capture_recorded_progress() {
    struct Capture { app: Gui, _runtime: tokio::runtime::Runtime, frames: usize, output: std::path::PathBuf }
    impl eframe::App for Capture {
        fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
            panel(ui, &mut self.app);
            let screenshot = ui.input(|input| input.events.iter().find_map(|event| {
                if let egui::Event::Screenshot { image, .. } = event { Some(image.clone()) } else { None }
            }));
            if let Some(image) = screenshot {
                let bytes: Vec<u8> = image.pixels.iter().flat_map(|pixel| pixel.to_array()).collect();
                image::save_buffer(&self.output, &bytes, image.width() as u32, image.height() as u32, image::ColorType::Rgba8).unwrap();
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else if self.frames == 3 {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            self.frames += 1;
            ui.ctx().request_repaint();
        }
    }
    let status: Value = serde_json::from_slice(&std::fs::read(std::env::var_os("HERBERT_STATUS_FIXTURE").expect("Select a status JSON file")).unwrap()).unwrap();
    let output = std::path::PathBuf::from(std::env::var_os("HERBERT_STATUS_CAPTURE").expect("Select a PNG output path"));
    let width = std::env::var("HERBERT_STATUS_WIDTH").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1180.0);
    eframe::run_native("Herbert - recorded status rendering", eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([width,820.0]),
        #[cfg(windows)]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        })),
        ..Default::default()
    }, Box::new(move |cc| {
        configure_style(&cc.egui_ctx);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut app = app(&cc.egui_ctx, &runtime, true);
        app.journal = Some(status);
        if std::env::var_os("HERBERT_STATUS_SETUP").is_some() { app.run_id.clear(); }
        Ok(Box::new(Capture { app, _runtime:runtime, frames:0, output }))
    })).unwrap();
}
#[test]
#[ignore = "Interactive synthetic preview; never loads personal state or connects."]
fn preview_download_progress() {
    struct Preview { app: Gui, _runtime: tokio::runtime::Runtime }
    impl eframe::App for Preview {
        fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) { panel(ui, &mut self.app); }
    }
    eframe::run_native("Herbert — width and wrapping preview", eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1360.0,900.0]).with_min_inner_size([720.0,560.0]).with_position([100.0,100.0]),
        #[cfg(windows)]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        })),
        ..Default::default()
    }, Box::new(|cc| {
        configure_style(&cc.egui_ctx);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        Ok(Box::new(Preview { app: app(&cc.egui_ctx, &runtime, true), _runtime: runtime }))
    })).unwrap();
}

#[test]
fn vacuum_moves_on_local_work_and_freezes_when_paused() {
    let runtime=tokio::runtime::Runtime::new().unwrap();
    let ctx=egui::Context::default();configure_style(&ctx);let mut app=app(&ctx,&runtime,true);
    let texture=app.brand_icon.id();
    let mut render=|time:f64,work:u64,paused:bool|{
        let mut status=fixture(true);status["statistics"]["meaningfulWork"]=json!(work);status["pauseRequested"]=json!(paused);
        status["statistics"]["decompilerJobs"]=json!({"running":2,"queued":3,"finished":4});
        app.journal=Some(status);
        let mut output=ctx.run_ui(egui::RawInput{time:Some(time),screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1180.0,820.0))),..Default::default()},|ui|{panel(ui,&mut app);});
        output.textures_delta.clear();
        // Measure the 0.5/0.8 rotation pivot; decorative rocking is not travel.
        output.shapes.iter().find_map(|s|if let egui::Shape::Mesh(mesh)=&s.shape{(mesh.texture_id==texture).then(||mesh.vertices.iter().map(|v|v.pos.x*(0.1+0.3*v.uv.y)).sum::<f32>()/mesh.vertices.iter().map(|v|0.1+0.3*v.uv.y).sum::<f32>())}else{None}).expect("Vacuum must remain visible")
    };
    let mut first=0.0;for frame in 0..60{first=render(frame as f64/60.0,1,false);}
    let mut moved=0.0;for frame in 60..120{moved=render(frame as f64/60.0,9,false);}assert!((moved-first).abs()>1.0,"Local work must move the vacuum");
    let mut idle=0.0;for frame in 120..180{idle=render(frame as f64/60.0,9,false);}assert!((idle-moved).abs()<0.1,"After easing, elapsed time alone must not move travel");
    let paused=render(3.0,25,true);assert!((paused-idle).abs()<0.1);
    let mut resumed=0.0;for frame in 181..241{resumed=render(frame as f64/60.0,25,false);}assert!((resumed-paused).abs()>1.0);
    let mut recovered=0.0;for frame in 241..301{recovered=render(frame as f64/60.0,3,false);}assert!((recovered-resumed).abs()>1.0,"A recovered execution with reset counters must still move");
}

#[test]
fn retained_session_error_event_routes_next_download_to_cleanup() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let ctx = egui::Context::default();
    let root = tempfile::tempdir().unwrap();
    let mut app = app(&ctx, &runtime, false);
    app.host = Host::with_roots(root.path().join("state"), root.path().join("plugins"), Some(root.path().join("coordination")));
    app.run_id.clear();
    app.settings_loaded = true;
    app.profiles_loaded = true;
    app.url = "http://127.0.0.1:1/unused".into();
    app.company = "Fixture".into();
    app.username = "synthetic".into();
    app.password = "synthetic".into();
    app.destination = root.path().join("output").to_string_lossy().into_owned();
    app.categories.insert("ud-codes".into(), true);
    let mut error = Diagnostic::new("ADMISSION_FAILED", "Admission failed.");
    error.context.insert("connectionRetained".into(), json!(true));
    error.context.insert("cleanup".into(), json!(Diagnostic::new("LOGOUT_FAILED", "Logout failed.")));
    app.sender.send(Event::Started(Reply {schema_version:1,ok:false,result:None,error:Some(error)})).unwrap();
    app.receive(&ctx);
    assert!(!app.connected);
    assert!(app.cleanup_pending);
    assert!(app.notice.contains("Admission failed."));
    assert!(app.notice.contains("Signing out of Epicor also failed: Logout failed."));
    app.start_download(&ctx);
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.working && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        app.receive(&ctx);
    }
    assert!(!app.working);
    // This synthetic host has no actual session. Disconnect must therefore fail
    // before login can contact the deliberately unused endpoint.
    assert_eq!(app.diagnostic.as_deref(), Some("CONNECTION_REQUIRED"));
    assert!(app.cleanup_pending);
    app.sender.send(Event::Reply {method:"connection.disconnect".into(),generation:0,profile:String::new(),reply:Reply {schema_version:1,ok:true,result:Some(json!({"disconnected":true})),error:None}}).unwrap();
    app.receive(&ctx);
    assert!(!app.connected);
    assert!(!app.cleanup_pending);
    assert!(runtime.block_on(app.host.shutdown()).is_empty());
}

#[test]
fn gui_user_file_request_survives_shared_decoder_and_dispatcher() {
    use crate::input::{ConnectionInput, CredentialSource};
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let ctx = egui::Context::default();
    let root = tempfile::tempdir().unwrap();
    let mut app = app(&ctx, &runtime, false);
    app.host = Host::with_roots(root.path().join("state"), root.path().join("plugins"), Some(root.path().join("coordination")));
    let profile = Profile { id:"selected-environment".into(), name:None, url:"https://example.invalid/fixture/".into(), auth_mode:AuthMode::Session, defaults:Defaults::default(), user_file:None, allow_untrusted_certificate:false };
    app.source = Source::File;
    app.input_file = root.path().join("absent.json").to_string_lossy().into_owned();
    app.file_choices_path = app.input_file.clone();
    app.file_environment = profile.id.clone();
    app.file_choices = vec![profile.clone()];
    let settings = herbert_contract::OperatingSettings::default();
    let request = app.connection_arguments(&settings).unwrap();
    let decoded: ConnectionInput = serde_json::from_value(request.clone()).unwrap();
    match decoded.credential_source {
        CredentialSource::UserFile {path, environment_id} => {
            assert_eq!(path, app.input_file);
            assert_eq!(environment_id, app.file_environment);
        }
        _ => panic!("GUI file selection changed credential source"),
    }
    let reply = runtime.block_on(app.host.execute(Command {schema_version:1,method:"connection.connect".into(),arguments:request}));
    assert_eq!(reply.error.unwrap().code, "ENVIRONMENT_FILE_UNAVAILABLE");
    app.source = Source::Entered;
    app.url = profile.url.clone();
    app.username = "synthetic-user".into();
    app.password = "synthetic-password".into();
    let decoded: ConnectionInput = serde_json::from_value(app.connection_arguments(&settings).unwrap()).unwrap();
    match decoded.credential_source {
        CredentialSource::Inline {credentials} => {
            assert_eq!(credentials.username, app.username);
            assert_eq!(credentials.password, app.password);
        }
        _ => panic!("Entered credentials changed source"),
    }
    app.source = Source::Saved;
    app.profile_id = profile.id.clone();
    app.profiles = vec![profile];
    let decoded: ConnectionInput = serde_json::from_value(app.connection_arguments(&settings).unwrap()).unwrap();
    assert_eq!(decoded.profile_id.as_deref(), Some(app.profile_id.as_str()));
    assert!(matches!(decoded.credential_source, CredentialSource::SecureStore));
    assert!(runtime.block_on(app.host.shutdown()).is_empty());
}
