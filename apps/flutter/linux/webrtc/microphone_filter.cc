#include "microphone_filter.h"

#include <algorithm>
#include <array>
#include <condition_variable>
#include <cstring>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>

#include "../../native/audio_filter.h"

namespace waddle_audio {
namespace {

using namespace libwebrtc;
using Filter =
    std::unique_ptr<WaddleAudioFilter, decltype(&waddle_audio_filter_destroy)>;

// The WebRTC ADM delivers 10 ms blocks after its normal device processing.
struct Frame {
  std::array<int16_t, 960> pcm;
  int rate;
  size_t channels, frames;
};

class Capture final : public AudioTrackSink {
 public:
  Capture(std::string input_id, scoped_refptr<RTCAudioTrack> input,
          scoped_refptr<RTCAudioSource> output, uint32_t mode, Filter filter)
      : input_id(std::move(input_id)),
        input_(std::move(input)),
        output_(std::move(output)),
        mode_(mode),
        filter_(std::move(filter)),
        input_enabled_(input_->enabled()),
        worker_([this] { run(); }) {
    input_->AddSink(this);
  }

  ~Capture() override {
    input_->RemoveSink(this);
    {
      std::lock_guard<std::mutex> lock(mutex_);
      stopping_ = true;
      count_ = 0;
    }
    wake_.notify_one();
    worker_.join();
  }

  void OnData(const void* pcm, int bits, int rate, size_t channels,
              size_t frames) override {
    if (!pcm || bits != 16 || rate <= 0 || rate > 48000 || rate % 100 ||
        frames != static_cast<size_t>(rate / 100) ||
        (channels != 1 && channels != 2))
      return;
    std::lock_guard<std::mutex> lock(mutex_);
    if (stopping_ || !input_enabled_ || !output_enabled_) return;
    if (count_ == queue_.size()) {
      // Discard delayed speech if inference falls behind real time.
      count_ = 0;
      ++generation_;
    }
    auto& block = queue_[(head_ + count_++) % queue_.size()];
    block.rate = rate;
    block.channels = channels;
    block.frames = frames;
    std::memcpy(block.pcm.data(), pcm, frames * channels * sizeof(int16_t));
    wake_.notify_one();
  }

  void set_enabled(bool input, bool enabled) {
    std::lock_guard<std::mutex> lock(mutex_);
    bool& state = input ? input_enabled_ : output_enabled_;
    if (state == enabled) return;
    state = enabled;
    count_ = 0;
    ++generation_;
  }

  const std::string input_id;

 private:
  void run() {
    int rate = 48000;
    size_t channels = 1;
    uint64_t processed_generation = 0;
    while (true) {
      Frame block{};
      uint64_t generation;
      {
        std::unique_lock<std::mutex> lock(mutex_);
        wake_.wait(lock, [this] { return stopping_ || count_; });
        if (stopping_) return;
        block = queue_[head_];
        head_ = (head_ + 1) % queue_.size();
        --count_;
        generation = generation_;
      }
      if (rate != block.rate || channels != block.channels) {
        rate = block.rate;
        channels = block.channels;
        filter_.reset(waddle_audio_filter_create(mode_, rate, channels));
      } else if (filter_ && processed_generation != generation) {
        waddle_audio_filter_reset(filter_.get());
      }
      processed_generation = generation;
      if (!filter_ ||
          !waddle_audio_filter_process(filter_.get(), block.pcm.data(),
                                       block.frames * channels))
        std::fill_n(block.pcm.begin(), block.frames * channels, 0);

      std::lock_guard<std::mutex> lock(mutex_);
      if (!stopping_ && generation == generation_ && input_enabled_ &&
          output_enabled_)
        output_->CaptureFrame(block.pcm.data(), 16, rate, channels,
                              block.frames);
    }
  }

  scoped_refptr<RTCAudioTrack> input_;
  scoped_refptr<RTCAudioSource> output_;
  uint32_t mode_;
  Filter filter_;
  std::mutex mutex_;
  std::condition_variable wake_;
  std::array<Frame, 4> queue_{};
  size_t head_ = 0, count_ = 0;
  uint64_t generation_ = 0;
  bool stopping_ = false, input_enabled_, output_enabled_ = true;
  std::thread worker_;
};

// Flutter's platform thread owns the plugin's local track registry and these
// maps.
std::map<std::string, RTCPeerConnectionFactory*> microphones;
std::map<std::string, std::unique_ptr<Capture>> captures;
}  // namespace

bool requested(const EncodableMap& audio) {
  return audio.count(EncodableValue("waddleMicrophoneTrack")) ||
         audio.count(EncodableValue("waddleAudioFilter"));
}

void remember_microphone(const std::string& id,
                         RTCPeerConnectionFactory* owner) {
  microphones.emplace(id, owner);
}

bool create(const EncodableMap& audio, const std::string& id,
            RTCPeerConnectionFactory* factory, RTCMediaStream* stream,
            Tracks& tracks, EncodableMap& result) {
  const auto input_id = findString(audio, "waddleMicrophoneTrack");
  const auto input = tracks.find(input_id);
  const auto mic = microphones.find(input_id);
  if (input == tracks.end() || mic == microphones.end() ||
      mic->second != factory)
    return false;
  const auto name = findString(audio, "waddleAudioFilter");
  uint32_t mode;
  if (name == "rnnoise")
    mode = WADDLE_AUDIO_RNNOISE;
  else if (name == "dtln")
    mode = WADDLE_AUDIO_DTLN;
  else
    return false;

  Filter filter(waddle_audio_filter_create(mode, 48000, 1),
                waddle_audio_filter_destroy);
  if (!filter) return false;
  RTCAudioOptions options;
  options.echo_cancellation = false;
  options.auto_gain_control = false;
  options.noise_suppression = false;
  options.highpass_filter = false;
  auto source =
      factory->CreateAudioSource(id.c_str(), RTCAudioSource::kCustom, options);
  if (!source) return false;
  auto output = factory->CreateAudioTrack(source, id.c_str());
  if (!output) return false;
  auto* microphone = static_cast<RTCAudioTrack*>(input->second.get());
  auto capture = std::make_unique<Capture>(input_id, microphone, source, mode,
                                           std::move(filter));
  stream->AddTrack(output);
  captures.emplace(id, std::move(capture));
  tracks[id] = output;
  EncodableMap info;
  info[EncodableValue("id")] = EncodableValue(id);
  info[EncodableValue("kind")] = EncodableValue("audio");
  info[EncodableValue("label")] = EncodableValue("Filtered microphone");
  info[EncodableValue("enabled")] = EncodableValue(true);
  result[EncodableValue("audioTracks")] =
      EncodableValue(EncodableList{EncodableValue(info)});
  return true;
}

void set_enabled(const std::string& id, bool enabled) {
  for (auto& entry : captures) {
    if (entry.first == id)
      entry.second->set_enabled(false, enabled);
    else if (entry.second->input_id == id)
      entry.second->set_enabled(true, enabled);
  }
}

void forget(const std::string& id) {
  captures.erase(id);
  if (!microphones.erase(id)) return;
  for (auto it = captures.begin(); it != captures.end();) {
    if (it->second->input_id == id)
      it = captures.erase(it);
    else
      ++it;
  }
}

void clear(RTCPeerConnectionFactory* owner) {
  std::vector<std::string> retired;
  for (const auto& mic : microphones) {
    if (mic.second == owner) retired.push_back(mic.first);
  }
  for (const auto& id : retired) forget(id);
}
}  // namespace waddle_audio
