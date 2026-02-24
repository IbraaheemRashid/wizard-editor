pub type AudioProducer = ringbuf::HeapProd<f32>;

#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub pts_seconds: f64,
    pub width: u32,
    pub height: u32,
    pub rgba_data: Vec<u8>,
}
