package social.waddle.flutter;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;

final class BoundedInput {
    private BoundedInput() {}

    static final class LimitExceeded extends IOException {}

    static byte[] read(InputStream input, int limit) throws IOException {
        ByteArrayOutputStream output = new ByteArrayOutputStream();
        byte[] buffer = new byte[8192];
        int remaining = limit;
        while (true) {
            int count = input.read(buffer, 0, Math.min(buffer.length, remaining + 1));
            if (count < 0) return output.toByteArray();
            if (count == 0) {
                int value = input.read();
                if (value < 0) return output.toByteArray();
                if (remaining == 0) throw new LimitExceeded();
                output.write(value);
                remaining--;
                continue;
            }
            if (count > remaining) throw new LimitExceeded();
            output.write(buffer, 0, count);
            remaining -= count;
        }
    }
}
