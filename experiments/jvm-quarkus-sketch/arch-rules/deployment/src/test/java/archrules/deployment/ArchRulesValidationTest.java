package archrules.deployment;

import org.junit.jupiter.api.Assertions;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.extension.RegisterExtension;

import io.quarkus.test.QuarkusUnitTest;

/**
 * The STANDING liveness proof for Layer 3 — "a green build != the validator ran". A non-empty
 * quarkus-build-steps.list is necessary but NOT sufficient (the deploymentModule name could drift, the
 * step could be mis-wired, a #35110-style Kotlin regression could empty the list): only actually
 * observing augmentation FAIL on a real violation proves the validator is live. This test deploys a
 * synthetic app whose single bean ({@link BadAdminProvider}) violates the @ApplicationScoped rule and
 * asserts augmentation fails with OUR error. If the build step ever stops running, augmentation
 * SUCCEEDS and this test goes red — a permanent guard.
 *
 * <p>Why this proves it is OUR step: a @Dependent bean implementing an interface is legal CDI, so ArC
 * would NOT reject it on its own. The failure can only come from validateAdminProviders.
 */
public class ArchRulesValidationTest {

    @RegisterExtension
    static final QuarkusUnitTest test = new QuarkusUnitTest()
            .withApplicationRoot(jar -> jar.addClass(BadAdminProvider.class))
            .assertException(t -> {
                String chain = messageChain(t);
                Assertions.assertTrue(
                        chain.contains("arch-rules:") && chain.contains("not @ApplicationScoped"),
                        "Augmentation failed, but not with the arch-rules AdminDataProvider validation error. "
                                + "Chain was: " + chain);
            });

    @Test
    public void augmentationRejectsTheArchitectureViolation() {
        Assertions.fail("Augmentation should have failed on the arch-rules violation; this body must never run.");
    }

    /** Concatenate the whole cause chain — the augmentation failure may be wrapped before it reaches us. */
    private static String messageChain(Throwable t) {
        StringBuilder sb = new StringBuilder();
        for (Throwable c = t; c != null; c = c.getCause()) {
            sb.append(c.getClass().getName()).append(": ").append(c.getMessage()).append(" | ");
            if (c.getCause() == c) {
                break;
            }
        }
        return sb.toString();
    }
}
