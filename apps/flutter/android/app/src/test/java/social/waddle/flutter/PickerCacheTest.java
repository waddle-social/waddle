package social.waddle.flutter;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.io.File;
import java.nio.file.Files;
import java.util.List;
import org.junit.Rule;
import org.junit.Test;
import org.junit.rules.TemporaryFolder;

public final class PickerCacheTest {
    @Rule public TemporaryFolder cache = new TemporaryFolder();

    @Test
    public void removesOnlyTheSelectedCopy() throws Exception {
        File root = cache.newFolder("file_picker");
        File selection = new File(root, "1");
        assertTrue(selection.mkdir());
        File selected = new File(selection, "selected");
        File other = new File(selection, "other");
        assertTrue(selected.createNewFile());
        assertTrue(other.createNewFile());

        assertTrue(PickerCache.removeCopies(cache.getRoot(), List.of(selected.getPath())));
        assertFalse(selected.exists());
        assertTrue(other.exists());
    }

    @Test
    public void rejectsTraversalAndSymlinksOutsidePickerCache() throws Exception {
        File root = cache.newFolder("file_picker");
        File outside = cache.newFile("outside");
        File sibling = cache.newFolder("file_picker_sibling");
        File siblingFile = new File(sibling, "file");
        assertTrue(siblingFile.createNewFile());
        File link = new File(root, "link");
        Files.createSymbolicLink(link.toPath(), outside.toPath());

        assertFalse(PickerCache.removeCopies(cache.getRoot(), List.of(outside.getPath())));
        assertFalse(PickerCache.removeCopies(cache.getRoot(), List.of(siblingFile.getPath())));
        assertFalse(PickerCache.removeCopies(cache.getRoot(), List.of(link.getPath())));
        assertTrue(outside.exists());
        assertTrue(siblingFile.exists());
        assertTrue(Files.isSymbolicLink(link.toPath()));
    }

    @Test
    public void rejectsASymlinkedPickerCacheRoot() throws Exception {
        File outside = cache.newFolder("outside");
        File file = new File(outside, "file");
        assertTrue(file.createNewFile());
        File root = new File(cache.getRoot(), "file_picker");
        Files.createSymbolicLink(root.toPath(), outside.toPath());

        assertFalse(PickerCache.removeCopies(cache.getRoot(), List.of(new File(root, "file").getPath())));
        assertTrue(file.exists());
    }
}
