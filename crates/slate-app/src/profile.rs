//! Profile page: view and edit the signed-in user's public profile.

use std::sync::Mutex;

use base64::Engine;
use slate_core::commands::profile;
use slate_core::tidal_api::{ExternalLink, Profile};
use slint::{ComponentHandle, Image};

use crate::backend::Backend;
use crate::bootstrap::ProfileState;
use crate::images::tidal_image_url;

const WEBSITE: &str = "OFFICIAL_HOMEPAGE";
pub const SOCIALS: [&str; 5] = ["INSTAGRAM", "TIKTOK", "FACEBOOK", "TWITTER", "SNAPCHAT"];
/// Profile pictures are square; TIDAL accepts up to 1080.
const AVATAR_SIDE: u32 = 750;

pub type LoadedProfile = Mutex<Option<Profile>>;

/// (website, socials in `SOCIALS` order) from a link list.
pub fn split_links(links: &[ExternalLink]) -> (String, [String; 5]) {
    let mut website = String::new();
    let mut socials: [String; 5] = Default::default();
    for l in links {
        if l.link_type == WEBSITE {
            website = l.href.clone();
        } else if let Some(i) = SOCIALS.iter().position(|s| *s == l.link_type) {
            socials[i] = l.href.clone();
        }
    }
    (website, socials)
}

/// Rebuilds the link list; empty entries are dropped, website first.
pub fn assemble_links(website: &str, socials: &[String; 5]) -> Vec<ExternalLink> {
    let mut out = Vec::new();
    if !website.trim().is_empty() {
        out.push(ExternalLink {
            href: website.trim().to_string(),
            link_type: WEBSITE.into(),
        });
    }
    for (kind, value) in SOCIALS.iter().zip(socials) {
        if !value.trim().is_empty() {
            out.push(ExternalLink {
                href: value.trim().to_string(),
                link_type: (*kind).into(),
            });
        }
    }
    out
}

/// Centre-crops to a square, scales, and encodes as JPEG.
pub fn prepare_avatar(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("not an image: {e}"))?;
    let side = img.width().min(img.height());
    let square = img.crop_imm(
        (img.width() - side) / 2,
        (img.height() - side) / 2,
        side,
        side,
    );
    let scaled = square.resize_exact(
        side.min(AVATAR_SIDE),
        side.min(AVATAR_SIDE),
        image::imageops::FilterType::Lanczos3,
    );
    let mut out = Vec::new();
    scaled
        .to_rgb8()
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .map_err(|e| e.to_string())?;
    Ok(out)
}

impl Backend {
    fn profile_status(&self, text: impl Into<String>) {
        let text = text.into();
        self.ui(move |w| w.global::<ProfileState>().set_status(text.into()));
    }

    pub fn load_profile(&self) {
        self.ui(|w| {
            let g = w.global::<ProfileState>();
            g.set_loading(true);
            g.set_status("".into());
        });
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(uid)) = (this.state(), this.user_id().await) else {
                return;
            };
            match profile::get_profile(&state, uid).await {
                Ok(p) => {
                    let (website, socials) = split_links(&p.external_links);
                    let [instagram, tiktok, facebook, twitter, snapchat] = socials;
                    let pic = p
                        .picture_files
                        .iter()
                        .max_by_key(|f| f.width.unwrap_or(0))
                        .map(|f| f.href.clone());
                    let (name, handle, bio) = (
                        p.name.clone(),
                        p.handle.clone().unwrap_or_default(),
                        p.bio.clone().unwrap_or_default(),
                    );
                    let fans = p
                        .fan_count
                        .map(|n| format!("{n} followers"))
                        .unwrap_or_default();
                    let available = p.artist_id.is_some();
                    if let Ok(mut l) = this.profile.lock() {
                        *l = Some(p);
                    }
                    this.ui(move |w| {
                        let g = w.global::<ProfileState>();
                        g.set_available(available);
                        g.set_name(name.into());
                        g.set_handle(handle.into());
                        g.set_bio(bio.into());
                        g.set_website(website.into());
                        g.set_instagram(instagram.into());
                        g.set_tiktok(tiktok.into());
                        g.set_facebook(facebook.into());
                        g.set_twitter(twitter.into());
                        g.set_snapchat(snapchat.into());
                        g.set_fans(fans.into());
                        g.set_loading(false);
                    });
                    if let Some(url) = pic {
                        if let Some(px) = this.images.get(&state, &tidal_image_url(&url, 320)).await
                        {
                            this.ui(move |w| {
                                w.global::<ProfileState>().set_avatar(Image::from_rgba8(px))
                            });
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("Could not load profile: {}", e.log_safe());
                    this.ui(move |w| {
                        let g = w.global::<ProfileState>();
                        g.set_loading(false);
                        g.set_available(false);
                        g.set_status(msg.into());
                    });
                }
            }
        });
    }

    pub fn save_profile(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let Some(old) = this.profile.lock().ok().and_then(|l| l.clone()) else {
                return;
            };
            let Some(artist_id) = old.artist_id else {
                return;
            };
            let form = this
                .read_ui(|w| {
                    let g = w.global::<ProfileState>();
                    (
                        g.get_name().to_string(),
                        g.get_handle().to_string(),
                        g.get_bio().to_string(),
                        g.get_website().to_string(),
                        [
                            g.get_instagram().to_string(),
                            g.get_tiktok().to_string(),
                            g.get_facebook().to_string(),
                            g.get_twitter().to_string(),
                            g.get_snapchat().to_string(),
                        ],
                    )
                })
                .await;
            let (name, handle, bio, website, socials) = form;
            let mut errors: Vec<String> = Vec::new();
            let old_handle = old.handle.clone().unwrap_or_default();
            if name != old.name || handle != old_handle {
                let n = (name != old.name).then(|| name.clone());
                let h = (handle != old_handle).then(|| handle.clone());
                if let Err(e) = profile::update_profile_meta(&state, artist_id, n, h, false).await {
                    errors.push(format!("name/handle: {}", e.log_safe()));
                }
            }
            if bio != old.bio.clone().unwrap_or_default() {
                match &old.bio_id {
                    Some(id) => {
                        if let Err(e) = profile::update_profile_bio(&state, id.clone(), bio).await {
                            errors.push(format!("bio: {}", e.log_safe()));
                        }
                    }
                    None => errors.push("bio: this profile has no bio slot yet".into()),
                }
            }
            let links = assemble_links(&website, &socials);
            if links.len() != old.external_links.len()
                || links
                    .iter()
                    .zip(&old.external_links)
                    .any(|(a, b)| a.href != b.href || a.link_type != b.link_type)
            {
                if let Err(e) = profile::update_profile_links(&state, artist_id, links).await {
                    errors.push(format!("links: {}", e.log_safe()));
                }
            }
            if errors.is_empty() {
                this.profile_status("Profile saved.");
                this.load_profile();
            } else {
                this.profile_status(format!("Could not save {}", errors.join("; ")));
            }
        });
    }

    pub fn pick_profile_picture(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let Some(artist_id) = this
                .profile
                .lock()
                .ok()
                .and_then(|l| l.as_ref().and_then(|p| p.artist_id))
            else {
                return;
            };
            let file = rfd::AsyncFileDialog::new()
                .set_title("Choose a profile picture")
                .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                .pick_file()
                .await;
            let Some(file) = file else { return };
            let bytes = file.read().await;
            let jpeg = tokio::task::spawn_blocking(move || prepare_avatar(&bytes)).await;
            let jpeg = match jpeg {
                Ok(Ok(j)) => j,
                Ok(Err(e)) => return this.profile_status(format!("Could not use that image: {e}")),
                Err(e) => return this.profile_status(e.to_string()),
            };
            let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg);
            match profile::upload_profile_picture(&state, artist_id, b64).await {
                Ok(()) => {
                    this.profile_status("Picture updated.");
                    this.load_profile();
                }
                Err(e) => this.profile_status(format!("Upload failed: {}", e.log_safe())),
            }
        });
    }

    pub fn remove_profile_picture(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let Some(artist_id) = this
                .profile
                .lock()
                .ok()
                .and_then(|l| l.as_ref().and_then(|p| p.artist_id))
            else {
                return;
            };
            match profile::delete_profile_picture(&state, artist_id).await {
                Ok(()) => {
                    this.ui(|w| w.global::<ProfileState>().set_avatar(Image::default()));
                    this.profile_status("Picture removed.");
                }
                Err(e) => this.profile_status(format!("Could not remove: {}", e.log_safe())),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_split_and_reassemble() {
        let links = vec![
            ExternalLink {
                href: "https://x.test".into(),
                link_type: "OFFICIAL_HOMEPAGE".into(),
            },
            ExternalLink {
                href: "@me".into(),
                link_type: "TWITTER".into(),
            },
            ExternalLink {
                href: "ignored".into(),
                link_type: "OTHER".into(),
            },
        ];
        let (site, socials) = split_links(&links);
        assert_eq!(site, "https://x.test");
        assert_eq!(socials[3], "@me");
        let back = assemble_links(&site, &socials);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].link_type, "OFFICIAL_HOMEPAGE");
        assert_eq!(back[1].link_type, "TWITTER");
        assert!(assemble_links("  ", &Default::default()).is_empty());
    }

    #[test]
    fn avatar_is_cropped_square_jpeg() {
        let mut png = Vec::new();
        image::RgbImage::new(40, 20)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("png");
        let jpeg = prepare_avatar(&png).expect("jpeg");
        let decoded = image::load_from_memory(&jpeg).expect("decode");
        assert_eq!((decoded.width(), decoded.height()), (20, 20));
        assert!(prepare_avatar(b"nope").is_err());
    }
}
