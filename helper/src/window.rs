//! The helper's window, drawn with SDL3's renderer and its built-in 8x8 font: the frames to show,
//! the camera's picture for aiming, what has been read so far, and Cancel. No Qt.

use std::ffi::CString;
use std::ptr::{null, null_mut};

use sdl3_sys::everything::*;

use crate::decode::Gray;
use crate::protocol::Frame;

pub struct View<'a> {
    pub frame: Option<&'a Frame>,
    pub preview: Option<&'a Gray>,
    pub scanning: bool,
    /// A code was in the camera's view a moment ago.
    pub seeing: bool,
    pub lines: &'a [String],
}

pub struct Window {
    win: *mut SDL_Window,
    ren: *mut SDL_Renderer,
    tex: *mut SDL_Texture,
    tex_size: (i32, i32),
    cancel: SDL_FRect,
}

const BAR: f32 = 64.0;

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', " ")).unwrap_or_default()
}

impl Window {
    /// SDL's video must be initialised.
    pub fn open(title: &str) -> Result<Window, String> {
        let mut win = null_mut();
        let mut ren = null_mut();
        let t = cstr(title);
        // SAFETY: both out-pointers are valid; SDL fills them on success.
        let ok = unsafe {
            SDL_CreateWindowAndRenderer(
                t.as_ptr(),
                900,
                560,
                SDL_WINDOW_RESIZABLE,
                &mut win,
                &mut ren,
            )
        };
        if !ok {
            return Err(crate::camera::sdl_error());
        }
        // SAFETY: the window was just created.
        unsafe {
            SDL_SetWindowMinimumSize(win, 480, 360);
            SDL_RaiseWindow(win);
        }
        Ok(Window {
            win,
            ren,
            tex: null_mut(),
            tex_size: (0, 0),
            cancel: SDL_FRect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
        })
    }

    pub fn set_title(&self, title: &str) {
        let t = cstr(title);
        // SAFETY: the window is alive for as long as `self`.
        unsafe { SDL_SetWindowTitle(self.win, t.as_ptr()) };
    }

    pub fn hits_cancel(&self, x: f32, y: f32) -> bool {
        let c = &self.cancel;
        x >= c.x && x <= c.x + c.w && y >= c.y && y <= c.y + c.h
    }

    pub fn draw(&mut self, v: &View) {
        // SAFETY: every call below takes the renderer this value owns.
        unsafe {
            let (mut w, mut h) = (0, 0);
            SDL_GetCurrentRenderOutputSize(self.ren, &mut w, &mut h);
            let (w, h) = (w as f32, h as f32);
            SDL_SetRenderDrawColor(self.ren, 32, 36, 40, 255);
            SDL_RenderClear(self.ren);

            let body = SDL_FRect {
                x: 16.0,
                y: 16.0,
                w: w - 32.0,
                h: h - BAR - 32.0,
            };
            match (v.frame, v.scanning) {
                (Some(f), true) => {
                    let half = (body.w - 16.0) / 2.0;
                    self.qr(f, SDL_FRect { w: half, ..body });
                    self.camera(
                        v,
                        SDL_FRect {
                            x: body.x + half + 16.0,
                            w: half,
                            ..body
                        },
                    );
                }
                (Some(f), false) => self.qr(f, body),
                (None, true) => self.camera(v, body),
                (None, false) => {}
            }

            for (i, line) in v.lines.iter().take(2).enumerate() {
                self.text(16.0, h - BAR + 8.0 + i as f32 * 24.0, line, (230, 232, 235));
            }
            self.cancel = SDL_FRect {
                x: w - 136.0,
                y: h - BAR + 12.0,
                w: 120.0,
                h: 40.0,
            };
            SDL_SetRenderDrawColor(self.ren, 90, 96, 104, 255);
            SDL_RenderFillRect(self.ren, &self.cancel);
            self.text(
                self.cancel.x + 12.0,
                self.cancel.y + 12.0,
                "Cancel",
                (255, 255, 255),
            );
            SDL_RenderPresent(self.ren);
        }
    }

    /// A frame on white, with its quiet zone, in whole pixels a module so it stays sharp.
    unsafe fn qr(&self, f: &Frame, area: SDL_FRect) {
        let n = f.size as f32 + 8.0;
        let m = (area.w.min(area.h) / n).floor().max(1.0);
        let side = m * n;
        let x0 = area.x + ((area.w - side) / 2.0).floor();
        let y0 = area.y + ((area.h - side) / 2.0).floor();
        SDL_SetRenderDrawColor(self.ren, 255, 255, 255, 255);
        SDL_RenderFillRect(
            self.ren,
            &SDL_FRect {
                x: x0,
                y: y0,
                w: side,
                h: side,
            },
        );
        let mut dark = Vec::with_capacity(f.size * f.size / 2);
        for y in 0..f.size {
            for x in 0..f.size {
                if f.dark(x, y) {
                    dark.push(SDL_FRect {
                        x: x0 + (x as f32 + 4.0) * m,
                        y: y0 + (y as f32 + 4.0) * m,
                        w: m,
                        h: m,
                    });
                }
            }
        }
        SDL_SetRenderDrawColor(self.ren, 0, 0, 0, 255);
        SDL_RenderFillRects(self.ren, dark.as_ptr(), dark.len() as i32);
    }

    /// The camera's picture, fitted to the area, framed green while a code is in view.
    unsafe fn camera(&mut self, v: &View, area: SDL_FRect) {
        let Some(p) = v.preview else {
            self.text(
                area.x + 8.0,
                area.y + area.h / 2.0,
                "Waiting for the camera...",
                (200, 204, 210),
            );
            return;
        };
        let (pw, ph) = (p.w as i32, p.h as i32);
        if self.tex.is_null() || self.tex_size != (pw, ph) {
            if !self.tex.is_null() {
                SDL_DestroyTexture(self.tex);
            }
            self.tex = SDL_CreateTexture(
                self.ren,
                SDL_PIXELFORMAT_RGBA32,
                SDL_TEXTUREACCESS_STREAMING,
                pw,
                ph,
            );
            self.tex_size = (pw, ph);
        }
        if self.tex.is_null() {
            return;
        }
        let rgba: Vec<u8> = p.px.iter().flat_map(|&g| [g, g, g, 255]).collect();
        SDL_UpdateTexture(self.tex, null(), rgba.as_ptr().cast(), pw * 4);
        let scale = (area.w / p.w as f32).min(area.h / p.h as f32);
        let (dw, dh) = (p.w as f32 * scale, p.h as f32 * scale);
        let dst = SDL_FRect {
            x: area.x + (area.w - dw) / 2.0,
            y: area.y + (area.h - dh) / 2.0,
            w: dw,
            h: dh,
        };
        SDL_RenderTexture(self.ren, self.tex, null(), &dst);
        let (r, g, b) = if v.seeing {
            (60, 200, 110)
        } else {
            (120, 126, 134)
        };
        SDL_SetRenderDrawColor(self.ren, r, g, b, 255);
        for i in 0..3 {
            let k = i as f32;
            SDL_RenderRect(
                self.ren,
                &SDL_FRect {
                    x: dst.x - k,
                    y: dst.y - k,
                    w: dst.w + 2.0 * k,
                    h: dst.h + 2.0 * k,
                },
            );
        }
    }

    /// Text at twice the font's size.
    unsafe fn text(&self, x: f32, y: f32, s: &str, (r, g, b): (u8, u8, u8)) {
        let t = cstr(s);
        SDL_SetRenderDrawColor(self.ren, r, g, b, 255);
        SDL_SetRenderScale(self.ren, 2.0, 2.0);
        SDL_RenderDebugText(self.ren, x / 2.0, y / 2.0, t.as_ptr());
        SDL_SetRenderScale(self.ren, 1.0, 1.0);
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: each object was created by this value and is destroyed once, texture first.
        unsafe {
            if !self.tex.is_null() {
                SDL_DestroyTexture(self.tex);
            }
            SDL_DestroyRenderer(self.ren);
            SDL_DestroyWindow(self.win);
        }
    }
}
