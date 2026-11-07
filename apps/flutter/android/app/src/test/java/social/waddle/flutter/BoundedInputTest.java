package social.waddle.flutter;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

import java.io.ByteArrayInputStream;
import org.junit.Test;

public final class BoundedInputTest {
    @Test
    public void acceptsTheExactLimit() throws Exception {
        byte[] data = {1, 2, 3};
        assertArrayEquals(data, BoundedInput.read(new ByteArrayInputStream(data), data.length));
    }

    @Test
    public void rejectsAfterReadingAtMostOneBytePastTheLimit() {
        ByteArrayInputStream input = new ByteArrayInputStream(new byte[] {1, 2, 3, 4, 5});
        assertThrows(BoundedInput.LimitExceeded.class, () -> BoundedInput.read(input, 3));
        assertEquals(1, input.available());
    }
}
