pub struct AudioProducer(pub rtrb::Producer<f32>);
pub struct AudioConsumer(pub rtrb::Consumer<f32>);

pub struct RingPair;

impl RingPair {
    pub fn new(capacity: usize) -> (AudioProducer, AudioConsumer) {
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(capacity);
        (AudioProducer(producer), AudioConsumer(consumer))
    }
}

impl AudioProducer {
    /// Push samples from a slice. Drops any samples that don't fit (never blocks).
    pub fn push_slice(&mut self, samples: &[f32]) {
        let available = self.0.slots();
        let to_write = samples.len().min(available);
        if to_write == 0 {
            return;
        }
        if let Ok(mut chunk) = self.0.write_chunk_uninit(to_write) {
            let (s1, s2) = chunk.as_mut_slices();
            let split = s1.len();
            for (dst, &src) in s1.iter_mut().zip(samples[..split].iter()) {
                dst.write(src);
            }
            for (dst, &src) in s2.iter_mut().zip(samples[split..to_write].iter()) {
                dst.write(src);
            }
            unsafe { chunk.commit(to_write) };
        }
    }
}

impl AudioConsumer {
    /// Read up to `count` samples into `buf`. Returns number of samples read.
    pub fn read_into(&mut self, buf: &mut [f32]) -> usize {
        let available = self.0.slots();
        let to_read = buf.len().min(available);
        if to_read == 0 {
            return 0;
        }
        if let Ok(chunk) = self.0.read_chunk(to_read) {
            let (s1, s2) = chunk.as_slices();
            let split = s1.len();
            buf[..split].copy_from_slice(s1);
            buf[split..to_read].copy_from_slice(s2);
            chunk.commit_all();
            to_read
        } else {
            0
        }
    }
}
