import 'android_screen_capture_test.dart' as screen_capture;
import 'standalone_smoke_test.dart' as standalone;
import 'story_camera_capture_test.dart' as story_camera;

void main() {
  standalone.main();
  screen_capture.main();
  story_camera.main();
}
