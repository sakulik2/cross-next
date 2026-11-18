//! 封面解码：JPEG/PNG 字节 → 预乘 BGRA。走系统自带的 WIC，不引图片库。
//!
//! 在网络线程（MTA）上跑，不占 UI 线程 —— 解一张大图要几毫秒到几十毫秒，
//! 放在消息循环里会让拖动窗口时卡一下。

use crate::pixels::Image;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::Interface;

/// 解码并把长边缩到不超过 `max`。认不出的格式返回 `None`。
///
/// 先在 WIC 里缩（Fant 插值，质量好），因为 SMTC 给的封面可能是上千像素的原图，
/// 整张转成 BGRA 再自己缩既慢又占内存。
pub fn decode(bytes: &[u8], max: u32) -> Option<Image> {
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = SHCreateMemStream(Some(bytes))?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;

        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 {
            return None;
        }

        let source: IWICBitmapSource = if w.max(h) > max {
            let k = max as f64 / w.max(h) as f64;
            let sw = ((w as f64 * k).round() as u32).max(1);
            let sh = ((h as f64 * k).round() as u32).max(1);
            let scaler = factory.CreateBitmapScaler().ok()?;
            scaler
                .Initialize(&frame, sw, sh, WICBitmapInterpolationModeFant)
                .ok()?;
            (w, h) = (sw, sh);
            scaler.cast().ok()?
        } else {
            frame.cast().ok()?
        };

        let converter = factory.CreateFormatConverter().ok()?;
        converter
            .Initialize(
                &source,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .ok()?;

        let mut img = Image::new(w as usize, h as usize);
        converter
            .CopyPixels(std::ptr::null(), w * 4, &mut img.px)
            .ok()?;
        Some(img)
    }
}
