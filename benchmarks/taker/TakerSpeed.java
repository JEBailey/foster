import io.github.parseworks.taker.Taker;
import io.github.parseworks.taker.parsers.Chars;
import io.github.parseworks.taker.parsers.Lexical;
import io.github.parseworks.taker.parsers.Numeric;
import java.util.function.IntToLongFunction;

public class TakerSpeed {
    private static volatile long sink;
    static long numbers(Taker<Long> parser, String text, int count) {
        long sum = 0;
        for (int i = 0; i < count; i++) sum += parser.parseAll(text).value();
        return sum;
    }
    static long strings(Taker<String> parser, String text, int count) {
        long sum = 0;
        for (int i = 0; i < count; i++) sum += parser.parseAll(text).value().length();
        return sum;
    }
    static void measure(String name, long expected, IntToLongFunction workload) {
        // Warm the JVM separately from the reported samples.
        long warmUntil = System.nanoTime() + 2_000_000_000L;
        do { sink = workload.applyAsLong(1024); } while (System.nanoTime() < warmUntil);
        int count = 1;
        while (true) {
            long start = System.nanoTime();
            long sum = workload.applyAsLong(count);
            long elapsed = System.nanoTime() - start;
            if (sum != expected * count) throw new AssertionError(name);
            sink = sum;
            if (elapsed >= 100_000_000L) break;
            count *= 2;
            if (count <= 0) throw new AssertionError("calibration overflow");
        }
        for (int i = 0; i < 5; i++) {
            long start = System.nanoTime();
            long sum = workload.applyAsLong(count);
            long elapsed = System.nanoTime() - start;
            if (sum != expected * count) throw new AssertionError(name);
            sink = sum;
            System.out.println(name + "," + count + "," + elapsed + "," + sum);
        }
    }
    public static void main(String[] args) {
        var literal = Lexical.string("abcdefghijklmnop");
        measure("literal16", 16, n -> strings(literal, "abcdefghijklmnop", n));
        var number = Numeric.longValue;
        measure("integer9", 123456789, n -> numbers(number, "123456789", n));
        var scan = Chars.takeWhile(c -> c >= 'a' && c <= 'z');
        var text = "abcdefghijklmnopqrstuvwxyz".repeat(9) + "abcdefghijklmnopqrstuv";
        if (text.length() != 256) throw new AssertionError();
        measure("scan256", 256, n -> strings(scan, text, n));
        var trimmed = Lexical.trimWhitespace(Numeric.longValue);
        measure("trimmed_integer", 123456789, n -> numbers(trimmed, " \t123456789\r\n ", n));
    }
}
