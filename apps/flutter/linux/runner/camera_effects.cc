#include "camera_effects.h"

#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

#include "../../native/camera_effect.h"
#include "flutter_webrtc.h"
#include "flutter_webrtc/flutter_web_r_t_c_plugin.h"
#include "rtc_video_processor.h"

namespace {
using libwebrtc::RTCVideoFrame;
using libwebrtc::scoped_refptr;

class CameraProcessor final : public libwebrtc::RTCVideoProcessor {
 public:
  bool configure(const uint8_t* rgba, size_t bytes, uint32_t width,
                 uint32_t height) {
    auto* next = waddle_camera_effect_create(rgba, bytes, width, height);
    if (!next) return false;
    std::lock_guard<std::mutex> lock(mutex_);
    effect_.reset(next);
    return true;
  }

  scoped_refptr<RTCVideoFrame> Process(
      scoped_refptr<RTCVideoFrame> frame) override {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!effect_ || !frame) return nullptr;
    const int w = frame->width(), h = frame->height();
    if (w <= 0 || h <= 0 || w > 4096 || h > 4096 || w * h > 4096 * 2160)
      return nullptr;
    const int cw = (w + 1) / 2, ch = (h + 1) / 2;
    const int sy = frame->StrideY(), su = frame->StrideU(),
              sv = frame->StrideV();
    if (sy < w || su < cw || sv < cw) return nullptr;
    const WaddleCameraInputPlane input[] = {
        {frame->DataY(), static_cast<size_t>(sy) * (h - 1) + w,
         static_cast<size_t>(sy)},
        {frame->DataU(), static_cast<size_t>(su) * (ch - 1) + cw,
         static_cast<size_t>(su)},
        {frame->DataV(), static_cast<size_t>(sv) * (ch - 1) + cw,
         static_cast<size_t>(sv)},
    };
    const size_t y_bytes = static_cast<size_t>(w) * h;
    const size_t uv_bytes = static_cast<size_t>(cw) * ch;
    try {
      pixels_.resize(y_bytes + 2 * uv_bytes);
    } catch (const std::bad_alloc&) {
      return nullptr;
    }
    auto* y = pixels_.data();
    auto* u = y + y_bytes;
    auto* v = u + uv_bytes;
    const WaddleCameraOutputPlane output[] = {
        {y, y_bytes, static_cast<size_t>(w)},
        {u, uv_bytes, static_cast<size_t>(cw)},
        {v, uv_bytes, static_cast<size_t>(cw)},
    };
    // The WebRTC source carries the original timestamp and rotation to the
    // replacement frame, whose sample layout remains in camera orientation.
    if (!waddle_camera_effect_process(effect_.get(), w, h, frame->rotation(),
                                      input, output))
      return nullptr;
    return RTCVideoFrame::Create(w, h, y, w, u, cw, v, cw);
  }

 private:
  std::mutex mutex_;
  std::unique_ptr<WaddleCameraEffect, decltype(&waddle_camera_effect_destroy)>
      effect_{nullptr, waddle_camera_effect_destroy};
  std::vector<uint8_t> pixels_;
};

struct Effects {
  std::map<std::string, std::unique_ptr<CameraProcessor>> attached;

  void remove(const std::string& id) {
    auto it = attached.find(id);
    if (it == attached.end()) return;
    auto* rtc = flutter_webrtc_plugin_get_shared_instance();
    if (rtc) rtc->RemoveVideoProcessor(id, it->second.get());
    // RemoveVideoProcessor waits for outstanding callbacks. Disposing a track
    // also synchronously detaches its processors.
    attached.erase(it);
  }

  void clear() {
    while (!attached.empty()) remove(attached.begin()->first);
  }
};

void set_effect(FlMethodChannel*, FlMethodCall* call, gpointer data) {
  if (g_strcmp0(fl_method_call_get_name(call), "set") != 0) {
    fl_method_call_respond_not_implemented(call, nullptr);
    return;
  }
  auto fail = [&](const char* message) {
    fl_method_call_respond_error(call, "camera-effect", message, nullptr,
                                 nullptr);
  };
  auto* args = fl_method_call_get_args(call);
  if (fl_value_get_type(args) != FL_VALUE_TYPE_MAP)
    return fail("Missing camera effect");
  auto field = [&](const char* key, FlValueType type) -> FlValue* {
    auto* v = fl_value_lookup_string(args, key);
    return v && fl_value_get_type(v) == type ? v : nullptr;
  };
  auto* id_value = field("trackId", FL_VALUE_TYPE_STRING);
  auto* mode_value = field("mode", FL_VALUE_TYPE_STRING);
  if (!id_value || !mode_value || !*fl_value_get_string(id_value))
    return fail("Missing camera track or effect mode");
  const std::string id = fl_value_get_string(id_value);
  const std::string mode = fl_value_get_string(mode_value);
  auto* effects = static_cast<Effects*>(data);
  if (mode == "off") {
    effects->remove(id);
    fl_method_call_respond_success(call, nullptr, nullptr);
    return;
  }
  const uint8_t* rgba = nullptr;
  size_t bytes = 0;
  uint32_t width = 0, height = 0;
  if (mode == "image") {
    auto* pixels = field("rgba", FL_VALUE_TYPE_UINT8_LIST);
    auto* w = field("width", FL_VALUE_TYPE_INT);
    auto* h = field("height", FL_VALUE_TYPE_INT);
    if (!pixels || !w || !h || fl_value_get_int(w) <= 0 ||
        fl_value_get_int(h) <= 0 || fl_value_get_int(w) > 1024 ||
        fl_value_get_int(h) > 1024) {
      return fail("Invalid background image");
    }
    width = fl_value_get_int(w);
    height = fl_value_get_int(h);
    bytes = fl_value_get_length(pixels);
    if (bytes != static_cast<size_t>(width) * height * 4) {
      return fail("Invalid background image size");
    }
    rgba = fl_value_get_uint8_list(pixels);
  } else if (mode != "blur") {
    return fail("Unknown camera effect");
  }
  auto it = effects->attached.find(id);
  if (it == effects->attached.end()) {
    auto* rtc = flutter_webrtc_plugin_get_shared_instance();
    auto processor = std::make_unique<CameraProcessor>();
    // The LiveKit caller selects its local camera. The native attachment
    // rejects remote, audio and already-disposed track identifiers.
    if (!rtc || !rtc->AddVideoProcessor(id, processor.get())) {
      return fail("The local camera is no longer available");
    }
    it = effects->attached.emplace(id, std::move(processor)).first;
  }
  // An attached processor drops frames until the model is ready.
  if (!it->second->configure(rgba, bytes, width, height))
    return fail("Could not initialize the camera effect");
  fl_method_call_respond_success(call, nullptr, nullptr);
}
}  // namespace

void register_camera_effects(FlView* view) {
  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  g_autoptr(FlMethodChannel) channel = fl_method_channel_new(
      fl_engine_get_binary_messenger(fl_view_get_engine(view)),
      "social.waddle.flutter/camera_effects", FL_METHOD_CODEC(codec));
  auto* effects = new Effects();
  g_object_set_data_full(
      G_OBJECT(channel), "camera-effects", effects,
      [](gpointer data) { delete static_cast<Effects*>(data); });
  // Stop callbacks while the window still owns its engine and WebRTC plugin.
  g_signal_connect_object(
      gtk_widget_get_toplevel(GTK_WIDGET(view)), "destroy",
      G_CALLBACK(+[](FlMethodChannel* channel) {
        static_cast<Effects*>(
            g_object_get_data(G_OBJECT(channel), "camera-effects"))
            ->clear();
      }),
      channel, G_CONNECT_SWAPPED);
  fl_method_channel_set_method_call_handler(channel, set_effect, effects,
                                            nullptr);
}
