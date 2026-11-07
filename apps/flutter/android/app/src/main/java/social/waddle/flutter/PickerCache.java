package social.waddle.flutter;

import java.io.File;
import java.io.IOException;
import java.util.List;

/** Removes only returned files within the picker plugin's private cache. */
final class PickerCache {
    private PickerCache() {}

    static boolean removeCopies(File cacheDir, List<?> paths) {
        try {
            File root = new File(cacheDir.getCanonicalFile(), "file_picker");
            if (!root.getCanonicalFile().equals(root)) return false;
            String prefix = root.getPath() + File.separator;
            boolean removed = true;
            for (Object path : paths) {
                if (!(path instanceof String)) {
                    removed = false;
                    continue;
                }
                File file = new File((String) path).getCanonicalFile();
                if (!file.getPath().startsWith(prefix)) {
                    removed = false;
                    continue;
                }
                if (file.exists() && (!file.isFile() || !file.delete())) {
                    removed = false;
                    continue;
                }
                File parent = file.getParentFile();
                if (parent != null && !parent.equals(root)) parent.delete();
            }
            return removed;
        } catch (IOException | SecurityException ignored) {
            return false;
        }
    }
}
