#ifndef WADDLE_LINUX_MICROPHONE_FILTER_H_
#define WADDLE_LINUX_MICROPHONE_FILTER_H_

#include <map>
#include <string>

#include "flutter_common.h"
#include "rtc_peerconnection_factory.h"

namespace waddle_audio {

using Tracks =
    std::map<std::string, libwebrtc::scoped_refptr<libwebrtc::RTCMediaTrack>>;

bool requested(const EncodableMap& audio);
void remember_microphone(const std::string& id,
                         libwebrtc::RTCPeerConnectionFactory* owner);
bool create(const EncodableMap& audio, const std::string& id,
            libwebrtc::RTCPeerConnectionFactory* factory,
            libwebrtc::RTCMediaStream* stream, Tracks& tracks,
            EncodableMap& result);
void set_enabled(const std::string& id, bool enabled);
void forget(const std::string& id);
void clear(libwebrtc::RTCPeerConnectionFactory* owner);

}  // namespace waddle_audio

#endif
