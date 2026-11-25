import 'android_audio_filter_test.dart' as audio_filter;
import 'android_call_window_test.dart' as call_window;
import 'android_camera_effect_test.dart' as camera_effect;
import 'android_screen_capture_test.dart' as screen_capture;
import 'standalone_smoke_test.dart' as standalone;
import 'story_camera_capture_test.dart' as story_camera;

void main() {
  standalone.main();
  screen_capture.main();
  story_camera.main();
  camera_effect.main();
  audio_filter.main();
  call_window.main();
}
