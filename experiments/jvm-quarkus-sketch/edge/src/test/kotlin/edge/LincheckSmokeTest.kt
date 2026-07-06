package edge

import java.util.concurrent.atomic.AtomicInteger
import org.jetbrains.kotlinx.lincheck.annotations.Operation
import org.jetbrains.kotlinx.lincheck.check
import org.jetbrains.kotlinx.lincheck.strategy.managed.modelchecking.ModelCheckingOptions
import org.junit.jupiter.api.Test

/**
 * De-risk: does Lincheck's bytecode instrumentation + model checking even RUN under JDK 26? A trivially
 * thread-safe counter — a green here means the framework works on this JVM (with whatever jvmArgs the
 * test task grants); the real cache test then follows.
 */
class LincheckSmokeTest {
    private val value = AtomicInteger(0)

    @Operation fun inc(): Int = value.incrementAndGet()

    @Operation fun get(): Int = value.get()

    @Test
    fun modelChecking() {
        ModelCheckingOptions().check(this::class)
    }
}
