// SPDX-License-Identifier: GPL-3.0-or-later
use crate::database::{icons_dir_path, sanitize_id_stem, Tracker};
use gtk::{cairo, gdk, gdk_pixbuf, glib, prelude::*};
use std::{cell::RefCell, path::Path, rc::Rc};

struct AvatarEditorState {
    selected_device_id: Option<String>,
    loaded_pixbuf: Option<gdk_pixbuf::Pixbuf>,
    zoom: f64,
    pan_x: f64,
    pan_y: f64,
    drag_start_x: f64,
    drag_start_y: f64,
    dragging: bool,
    circular_mask: bool,
}

impl AvatarEditorState {
    fn new() -> Self {
        Self {
            selected_device_id: None,
            loaded_pixbuf: None,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            drag_start_x: 0.0,
            drag_start_y: 0.0,
            dragging: false,
            circular_mask: false,
        }
    }

    fn reset_transform(&mut self) {
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        self.dragging = false;
    }
}

pub fn build_misc_tab(tracker: &Rc<RefCell<Tracker>>) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(18);
    root.set_margin_end(18);

    let title_lbl = gtk::Label::new(None);
    title_lbl.set_markup("<b>🎨 Miscelánea — Icono Personalizado / Foto de Perfil (PFP)</b>");
    title_lbl.set_halign(gtk::Align::Start);
    root.pack_start(&title_lbl, false, false, 0);

    let desc_lbl = gtk::Label::new(Some(
        "Selecciona un auricular registrado, carga una imagen (PNG, JPG, JPEG, JFIF) y encuádrala.",
    ));
    desc_lbl.set_halign(gtk::Align::Start);
    root.pack_start(&desc_lbl, false, false, 0);

    // --- Fila 1: Selector de auricular registrado ---
    let dev_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let dev_lbl = gtk::Label::new(Some("Auricular:"));
    let dev_combo = gtk::ComboBoxText::new();
    dev_combo.set_hexpand(true);
    let reload_devs_btn = gtk::Button::with_label("🔄 Actualizar lista");

    dev_row.pack_start(&dev_lbl, false, false, 0);
    dev_row.pack_start(&dev_combo, true, true, 0);
    dev_row.pack_start(&reload_devs_btn, false, false, 0);
    root.pack_start(&dev_row, false, false, 4);

    // --- Fila 2: Contenedor central (Lienzo interactivo + Controles) ---
    let center_box = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    center_box.set_margin_top(6);

    let canvas_col = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let view_size: f64 = 240.0;

    let area = gtk::DrawingArea::new();
    area.set_size_request(view_size as i32, view_size as i32);
    area.set_halign(gtk::Align::Center);
    area.add_events(
        gdk::EventMask::BUTTON_PRESS_MASK
            | gdk::EventMask::BUTTON_RELEASE_MASK
            | gdk::EventMask::POINTER_MOTION_MASK
            | gdk::EventMask::SCROLL_MASK,
    );
    canvas_col.pack_start(&area, false, false, 0);

    let zoom_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let zoom_lbl = gtk::Label::new(Some("🔍 Zoom:"));
    let zoom_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 4.0, 0.05);
    zoom_scale.set_value(1.0);
    zoom_scale.set_hexpand(true);
    zoom_row.pack_start(&zoom_lbl, false, false, 0);
    zoom_row.pack_start(&zoom_scale, true, true, 0);
    canvas_col.pack_start(&zoom_row, false, false, 0);

    let circle_chk = gtk::CheckButton::with_label("Recorte circular estilo foto de perfil (PFP)");
    canvas_col.pack_start(&circle_chk, false, false, 0);

    center_box.pack_start(&canvas_col, false, false, 0);

    // Columna derecha: Vista previa guardada y botones de acción
    let actions_col = gtk::Box::new(gtk::Orientation::Vertical, 10);
    actions_col.set_hexpand(true);

    let prev_title = gtk::Label::new(None);
    prev_title.set_markup("<b>Icono guardado actualmente:</b>");
    prev_title.set_halign(gtk::Align::Start);
    actions_col.pack_start(&prev_title, false, false, 0);

    let current_icon_img =
        gtk::Image::from_icon_name(Some("audio-headphones"), gtk::IconSize::Dialog);
    current_icon_img.set_pixel_size(64);
    current_icon_img.set_halign(gtk::Align::Start);
    actions_col.pack_start(&current_icon_img, false, false, 2);

    let status_lbl = gtk::Label::new(Some("Estado: Selecciona un dispositivo."));
    status_lbl.set_halign(gtk::Align::Start);
    status_lbl.set_line_wrap(true);
    actions_col.pack_start(&status_lbl, false, false, 4);

    let btn_choose = gtk::Button::with_label("📂 Seleccionar / Cambiar imagen...");
    let btn_save = gtk::Button::with_label("💾 Guardar icono ajustado");
    let btn_revert = gtk::Button::with_label("↩️ Revertir ajuste");
    let btn_delete = gtk::Button::with_label("🗑️ Eliminar icono personalizado");

    actions_col.pack_start(&btn_choose, false, false, 0);
    actions_col.pack_start(&btn_save, false, false, 0);
    actions_col.pack_start(&btn_revert, false, false, 0);
    actions_col.pack_start(&btn_delete, false, false, 0);

    center_box.pack_start(&actions_col, true, true, 0);
    root.pack_start(&center_box, false, false, 0);

    let state = Rc::new(RefCell::new(AvatarEditorState::new()));

    // Sincronizar selección del ComboBox con el estado y vista previa
    let sync_device_selection: Rc<dyn Fn()> = {
        let tracker = tracker.clone();
        let state = state.clone();
        let dev_combo = dev_combo.clone();
        let current_icon_img = current_icon_img.clone();
        let status_lbl = status_lbl.clone();
        let zoom_scale = zoom_scale.clone();
        let area = area.clone();
        let btn_delete = btn_delete.clone();

        Rc::new(move || {
            let active_id = dev_combo.active_id().map(|s| s.to_string());
            let mut st = state.borrow_mut();
            st.selected_device_id = active_id.clone();
            st.reset_transform();
            zoom_scale.set_value(1.0);

            if let Some(id) = active_id {
                let tr = tracker.borrow();
                if let Some(dev) = tr.db.devices.get(&id) {
                    if let Some(ref img_path) = dev.custom_image {
                        if Path::new(img_path).exists() {
                            if let Ok(pb64) =
                                gdk_pixbuf::Pixbuf::from_file_at_scale(img_path, 64, 64, true)
                            {
                                current_icon_img.set_from_pixbuf(Some(&pb64));
                            }
                            st.loaded_pixbuf = gdk_pixbuf::Pixbuf::from_file(img_path).ok();
                            status_lbl.set_text(
                                "Icono personalizado activo. Puedes reajustarlo o cambiarlo.",
                            );
                            btn_delete.set_sensitive(true);
                            area.queue_draw();
                            return;
                        }
                    }
                    current_icon_img
                        .set_from_icon_name(Some("audio-headphones"), gtk::IconSize::Dialog);
                    current_icon_img.set_pixel_size(64);
                    st.loaded_pixbuf = None;
                    status_lbl.set_text("Sin icono personalizado (usando icono por defecto).");
                    btn_delete.set_sensitive(false);
                }
            } else {
                st.loaded_pixbuf = None;
                btn_delete.set_sensitive(false);
            }
            area.queue_draw();
        })
    };

    // Poblar lista de auriculares registrados
    let populate_combo: Rc<dyn Fn()> = {
        let tracker = tracker.clone();
        let dev_combo = dev_combo.clone();
        let sync_sel = sync_device_selection.clone();

        Rc::new(move || {
            let prev_id = dev_combo.active_id().map(|s| s.to_string());
            dev_combo.remove_all();

            let tr = tracker.borrow();
            let mut list: Vec<_> = tr.db.devices.values().collect();
            list.sort_by(|a, b| b.lifetime_connected_seconds.cmp(&a.lifetime_connected_seconds));

            let mut first_id: Option<String> = None;
            for dev in list {
                if first_id.is_none() {
                    first_id = Some(dev.id.clone());
                }
                let label = format!("{} [{}] ({})", dev.name, dev.real_name, dev.id);
                dev_combo.append(Some(&dev.id), &label);
            }
            drop(tr);

            if let Some(pid) = prev_id {
                if !dev_combo.set_active_id(Some(&pid)) {
                    if let Some(fid) = first_id {
                        dev_combo.set_active_id(Some(&fid));
                    }
                }
            } else if let Some(fid) = first_id {
                dev_combo.set_active_id(Some(&fid));
            }
            sync_sel();
        })
    };

    populate_combo();

    {
        let sync_sel = sync_device_selection.clone();
        dev_combo.connect_changed(move |_| {
            sync_sel();
        });
    }

    {
        let pop = populate_combo.clone();
        reload_devs_btn.connect_clicked(move |_| {
            pop();
        });
    }

    // Dibujo del lienzo interactivo
    {
        let state = state.clone();
        area.connect_draw(move |_, cr| {
            cr.set_source_rgb(0.10, 0.10, 0.12);
            let _ = cr.paint();

            let st = state.borrow();
            if let Some(ref pb) = st.loaded_pixbuf {
                let ow = pb.width() as f64;
                let oh = pb.height() as f64;
                let base_scale = (view_size / ow).max(view_size / oh);
                let scale = base_scale * st.zoom;
                let sw = ow * scale;
                let sh = oh * scale;

                let max_px = ((sw - view_size) / 2.0).max(0.0);
                let max_py = ((sh - view_size) / 2.0).max(0.0);
                let cx = st.pan_x.clamp(-max_px, max_px);
                let cy = st.pan_y.clamp(-max_py, max_py);

                let draw_x = (view_size - sw) / 2.0 + cx;
                let draw_y = (view_size - sh) / 2.0 + cy;

                cr.save().ok();
                cr.translate(draw_x, draw_y);
                cr.scale(scale, scale);
                cr.set_source_pixbuf(pb, 0.0, 0.0);
                let _ = cr.paint();
                cr.restore().ok();

                if st.circular_mask {
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
                    cr.rectangle(0.0, 0.0, view_size, view_size);
                    cr.arc(
                        view_size / 2.0,
                        view_size / 2.0,
                        view_size * 0.46,
                        0.0,
                        std::f64::consts::PI * 2.0,
                    );
                    cr.set_fill_rule(cairo::FillRule::EvenOdd);
                    let _ = cr.fill();
                    cr.set_fill_rule(cairo::FillRule::Winding);

                    cr.set_source_rgba(0.96, 0.25, 0.37, 0.90);
                    cr.set_line_width(2.0);
                    cr.arc(
                        view_size / 2.0,
                        view_size / 2.0,
                        view_size * 0.46,
                        0.0,
                        std::f64::consts::PI * 2.0,
                    );
                    let _ = cr.stroke();
                } else {
                    cr.set_source_rgba(0.96, 0.25, 0.37, 0.85);
                    cr.set_line_width(2.0);
                    cr.rectangle(1.0, 1.0, view_size - 2.0, view_size - 2.0);
                    let _ = cr.stroke();
                }
            } else {
                cr.set_source_rgba(0.7, 0.7, 0.75, 0.7);
                cr.move_to(28.0, view_size / 2.0);
                let _ = cr.show_text("Carga una imagen para encuadrar");
            }

            glib::Propagation::Proceed
        });
    }

    // Eventos de ratón para arrastrar (pan) y rueda (zoom)
    {
        let state = state.clone();
        area.connect_button_press_event(move |_, ev| {
            if ev.button() == 1 {
                let (x, y) = ev.position();
                let mut st = state.borrow_mut();
                st.drag_start_x = x - st.pan_x;
                st.drag_start_y = y - st.pan_y;
                st.dragging = true;
            }
            glib::Propagation::Proceed
        });
    }

    {
        let state = state.clone();
        area.connect_button_release_event(move |_, ev| {
            if ev.button() == 1 {
                state.borrow_mut().dragging = false;
            }
            glib::Propagation::Proceed
        });
    }

    {
        let state = state.clone();
        let area_mot = area.clone();
        area.connect_motion_notify_event(move |_, ev| {
            let mut st = state.borrow_mut();
            if st.dragging {
                if let Some(ref pb) = st.loaded_pixbuf {
                    let ow = pb.width() as f64;
                    let oh = pb.height() as f64;
                    let base_scale = (view_size / ow).max(view_size / oh);
                    let scale = base_scale * st.zoom;
                    let max_px = ((ow * scale - view_size) / 2.0).max(0.0);
                    let max_py = ((oh * scale - view_size) / 2.0).max(0.0);
                    let (x, y) = ev.position();
                    st.pan_x = (x - st.drag_start_x).clamp(-max_px, max_px);
                    st.pan_y = (y - st.drag_start_y).clamp(-max_py, max_py);
                    area_mot.queue_draw();
                }
            }
            glib::Propagation::Proceed
        });
    }

    {
        let state = state.clone();
        let area_zoom = area.clone();
        zoom_scale.connect_value_changed(move |sc| {
            let z = sc.value();
            let mut st = state.borrow_mut();
            st.zoom = z;
            if let Some(ref pb) = st.loaded_pixbuf {
                let ow = pb.width() as f64;
                let oh = pb.height() as f64;
                let base_scale = (view_size / ow).max(view_size / oh);
                let scale = base_scale * z;
                let max_px = ((ow * scale - view_size) / 2.0).max(0.0);
                let max_py = ((oh * scale - view_size) / 2.0).max(0.0);
                st.pan_x = st.pan_x.clamp(-max_px, max_px);
                st.pan_y = st.pan_y.clamp(-max_py, max_py);
            }
            area_zoom.queue_draw();
        });
    }

    {
        let zoom_scale_scroll = zoom_scale.clone();
        area.connect_scroll_event(move |_, ev| {
            let cur = zoom_scale_scroll.value();
            match ev.direction() {
                gdk::ScrollDirection::Up => zoom_scale_scroll.set_value((cur + 0.12).min(4.0)),
                gdk::ScrollDirection::Down => zoom_scale_scroll.set_value((cur - 0.12).max(1.0)),
                _ => {}
            }
            glib::Propagation::Proceed
        });
    }

    {
        let state = state.clone();
        let area_chk = area.clone();
        circle_chk.connect_toggled(move |chk| {
            state.borrow_mut().circular_mask = chk.is_active();
            area_chk.queue_draw();
        });
    }

    // Botón 1: Seleccionar archivo (solo PNG, JPG, JPEG, JFIF)
    {
        let state = state.clone();
        let area_ch = area.clone();
        let zoom_scale_ch = zoom_scale.clone();
        let status_lbl_ch = status_lbl.clone();

        btn_choose.connect_clicked(move |btn| {
            let parent_win = btn
                .toplevel()
                .and_then(|w| w.downcast::<gtk::Window>().ok());
            let dlg = gtk::FileChooserDialog::with_buttons(
                Some("Seleccionar imagen (PNG, JPG, JPEG, JFIF)"),
                parent_win.as_ref(),
                gtk::FileChooserAction::Open,
                &[
                    ("Cancelar", gtk::ResponseType::Cancel),
                    ("Cargar imagen", gtk::ResponseType::Accept),
                ],
            );

            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Imágenes permitidas (PNG, JPG, JPEG, JFIF)"));
            for pat in &[
                "*.png", "*.PNG", "*.jpg", "*.JPG", "*.jpeg", "*.JPEG", "*.jfif", "*.JFIF",
            ] {
                filter.add_pattern(pat);
            }
            dlg.add_filter(filter);

            let chosen = if dlg.run() == gtk::ResponseType::Accept {
                dlg.filename()
            } else {
                None
            };
            unsafe {
                dlg.destroy();
            }

            if let Some(path) = chosen {
                if let Some(p_str) = path.to_str() {
                    match gdk_pixbuf::Pixbuf::from_file(p_str) {
                        Ok(pb) => {
                            let mut st = state.borrow_mut();
                            st.loaded_pixbuf = Some(pb);
                            st.reset_transform();
                            drop(st);
                            zoom_scale_ch.set_value(1.0);
                            status_lbl_ch.set_text(
                                "Imagen cargada en el lienzo. Encuádrala y pulsa 'Guardar icono ajustado'.",
                            );
                            area_ch.queue_draw();
                        }
                        Err(_) => {
                            status_lbl_ch.set_text("❌ No se pudo abrir el archivo de imagen.");
                        }
                    }
                }
            }
        });
    }

    // Botón 2: Guardar icono recortado 96x96
    {
        let tracker = tracker.clone();
        let state = state.clone();
        let sync_sel = sync_device_selection.clone();
        let status_lbl_sv = status_lbl.clone();

        btn_save.connect_clicked(move |_| {
            let st = state.borrow();
            let dev_id = match st.selected_device_id {
                Some(ref id) => id.clone(),
                None => {
                    status_lbl_sv.set_text("❌ Selecciona un auricular primero.");
                    return;
                }
            };
            let pb = match st.loaded_pixbuf {
                Some(ref p) => p.clone(),
                None => {
                    status_lbl_sv
                        .set_text("❌ Primero selecciona una imagen con el botón de arriba.");
                    return;
                }
            };

            let ow = pb.width() as f64;
            let oh = pb.height() as f64;
            let base_scale = (view_size / ow).max(view_size / oh);
            let scale = base_scale * st.zoom;
            let sw = ow * scale;
            let sh = oh * scale;
            let max_px = ((sw - view_size) / 2.0).max(0.0);
            let max_py = ((sh - view_size) / 2.0).max(0.0);
            let cx = st.pan_x.clamp(-max_px, max_px);
            let cy = st.pan_y.clamp(-max_py, max_py);
            let draw_x = (view_size - sw) / 2.0 + cx;
            let draw_y = (view_size - sh) / 2.0 + cy;
            let circular = st.circular_mask;
            drop(st);

            let out_size = 96;
            let dest_path = icons_dir_path().join(format!("{}.png", sanitize_id_stem(&dev_id)));

            if let Ok(surf) =
                cairo::ImageSurface::create(cairo::Format::ARgb32, out_size, out_size)
            {
                if let Ok(cr) = cairo::Context::new(&surf) {
                    let ratio = out_size as f64 / view_size;
                    cr.scale(ratio, ratio);
                    if circular {
                        cr.arc(
                            view_size / 2.0,
                            view_size / 2.0,
                            view_size * 0.46,
                            0.0,
                            std::f64::consts::PI * 2.0,
                        );
                        cr.clip();
                    }
                    cr.translate(draw_x, draw_y);
                    cr.scale(scale, scale);
                    cr.set_source_pixbuf(&pb, 0.0, 0.0);
                    let _ = cr.paint();
                }
                if let Some(cropped_pb) =
                    gdk::pixbuf_get_from_surface(&surf, 0, 0, out_size, out_size)
                {
                    if cropped_pb.savev(&dest_path, "png", &[]).is_ok() {
                        if let Some(saved_str) = dest_path.to_str() {
                            {
                                let mut tr = tracker.borrow_mut();
                                if let Some(dev) = tr.db.devices.get_mut(&dev_id) {
                                    dev.custom_image = Some(saved_str.to_string());
                                }
                                tr.save_db();
                            }
                            sync_sel();
                            status_lbl_sv.set_text("✔ Icono personalizado guardado y aplicado.");
                        }
                    }
                }
            }
        });
    }

    // Botón 3: Revertir ajuste
    {
        let sync_sel = sync_device_selection.clone();
        let status_lbl_rev = status_lbl.clone();
        btn_revert.connect_clicked(move |_| {
            sync_sel();
            status_lbl_rev.set_text("↩️ Ajustes revertidos al estado guardado.");
        });
    }

    // Botón 4: Eliminar icono personalizado
    {
        let tracker = tracker.clone();
        let state = state.clone();
        let sync_sel = sync_device_selection.clone();
        let status_lbl_del = status_lbl.clone();

        btn_delete.connect_clicked(move |_| {
            let dev_id = state.borrow().selected_device_id.clone();
            if let Some(id) = dev_id {
                tracker.borrow_mut().clear_custom_image(&id);
                sync_sel();
                status_lbl_del.set_text("🗑️ Icono personalizado eliminado correctamente.");
            }
        });
    }

    root
}
