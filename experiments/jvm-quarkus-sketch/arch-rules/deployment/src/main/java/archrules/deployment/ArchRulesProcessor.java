package archrules.deployment;

import java.util.ArrayList;
import java.util.List;

import jakarta.enterprise.inject.spi.DeploymentException;

import org.jboss.jandex.ClassInfo;
import org.jboss.jandex.DotName;
import org.jboss.jandex.Type;
import org.jboss.logging.Logger;

import io.quarkus.arc.deployment.ValidationPhaseBuildItem;
import io.quarkus.arc.deployment.ValidationPhaseBuildItem.ValidationErrorBuildItem;
import io.quarkus.arc.processor.BeanInfo;
import io.quarkus.deployment.annotations.BuildProducer;
import io.quarkus.deployment.annotations.BuildStep;
import io.quarkus.deployment.builditem.CombinedIndexBuildItem;
import io.quarkus.deployment.builditem.FeatureBuildItem;

/**
 * Verification Layer 3 (OPT-IN demo) — build-time architecture validation as Quarkus @BuildStep.
 *
 * <p>Re-implements Layer-1's architecture checks against ArC's AUGMENTED model rather than a raw jar
 * scan: the same invariants, but expressed as augmentation failures at {@code quarkusBuild}. Honest
 * demo-value framing — over ArC's own automatic ambiguous/unsatisfied resolution plus Layer 1's
 * resolved-classpath Jandex task, this adds almost no NET safety; its worth is (i) demonstrating
 * architecture-as-augmentation-failure in a Quarkus-native way and (ii) failing at build/augment time
 * with a clearer, named message.
 *
 * <p>Java, not Kotlin, ON PURPOSE: quarkus-extension-processor does not index Kotlin @BuildStep classes
 * (quarkusio/quarkus#35110), which would leave quarkus-build-steps.list empty and these validators
 * silently dead. Both steps CONSUME-only their inputs and PRODUCE-only ValidationErrorBuildItem
 * (collect-then-produce-once, never throw) to stay acyclic in the build graph (avoids #39660).
 */
public class ArchRulesProcessor {

    private static final Logger LOG = Logger.getLogger(ArchRulesProcessor.class);

    private static final String FEATURE = "arch-rules";

    // Domain types referenced by Jandex DotName string only — no compile dependency on any feature
    // module, so this validator stays fully decoupled from what it inspects.
    private static final DotName PLAYER_CHARACTERS = DotName.createSimple("characters.charactersapi.PlayerCharacters");
    private static final DotName ADMIN_DATA_PROVIDER = DotName.createSimple("admin.adminapi.AdminDataProvider");
    private static final DotName APPLICATION_SCOPED = DotName.createSimple("jakarta.enterprise.context.ApplicationScoped");

    /** Names the extension in the "Installed features" banner — a visible sign the extension loaded. */
    @BuildStep
    FeatureBuildItem feature() {
        return new FeatureBuildItem(FEATURE);
    }

    /**
     * Rule 1 — the PlayerCharacters capability must not be AMBIGUOUS. Consumes the AUGMENTED bean graph
     * (ArC's resolved model via ValidationPhaseBuildItem) and counts beans assignable to
     * {@code characters.charactersapi.PlayerCharacters}. More than one = ambiguous capability: fail with
     * a named message before ArC's generic "ambiguous dependencies for type X" fires at the injection
     * point.
     *
     * <p>We check {@code > 1}, not {@code != 1}, ON PURPOSE. A service that HOSTS the producer but does
     * not CONSUME the capability (e.g. {@code characters-service}, which stands up the local producer
     * for a remote peer but injects PlayerCharacters nowhere itself) has that produced bean pruned by
     * ArC's unused-bean removal BEFORE validation, so its augmented count is 0 — a legitimate topology,
     * not a violation. {@code != 1} would false-fail that real app-shell, i.e. ship a broken gate.
     * ArC already rejects the genuinely-unsatisfied case at the consuming injection point, so the
     * net-new value here is purely the clearer ambiguity message at augmentation time.
     */
    @BuildStep
    void validateSinglePlayerCharactersProducer(ValidationPhaseBuildItem validationPhase,
            BuildProducer<ValidationErrorBuildItem> errors) {
        Type pcType = Type.create(PLAYER_CHARACTERS, Type.Kind.CLASS);
        List<BeanInfo> beans = validationPhase.getContext().beans().assignableTo(pcType).collect();
        LOG.infof("arch-rules: %d bean(s) assignable to %s on the augmented graph", beans.size(), PLAYER_CHARACTERS);
        if (beans.size() > 1) {
            errors.produce(new ValidationErrorBuildItem(List.<Throwable>of(new DeploymentException(
                    "arch-rules: PlayerCharacters capability is AMBIGUOUS — " + beans.size()
                            + " beans are assignable to " + PLAYER_CHARACTERS + " on the augmented bean graph; "
                            + "exactly one producer is expected per service (a single local OR remote "
                            + "PlayerCharacters producer). Beans: " + beans))));
        }
    }

    /**
     * Rule 2 — every {@code admin.adminapi.AdminDataProvider} implementor must be @ApplicationScoped,
     * else it silently drops from the admin's {@code @All List<AdminDataProvider>}. Consumes the COMBINED
     * Jandex index (not the bean graph), so it is immune to unused-bean removal and sees every implementor
     * on the resolved classpath. Mirrors Layer 1's verifyAdminParity, now failing at augmentation.
     */
    @BuildStep
    void validateAdminProviders(CombinedIndexBuildItem index, BuildProducer<ValidationErrorBuildItem> errors) {
        List<Throwable> problems = new ArrayList<>();
        for (ClassInfo impl : index.getIndex().getAllKnownImplementors(ADMIN_DATA_PROVIDER)) {
            if (!impl.hasDeclaredAnnotation(APPLICATION_SCOPED)) {
                problems.add(new DeploymentException(
                        "arch-rules: " + impl.name() + " implements " + ADMIN_DATA_PROVIDER
                                + " but is not @ApplicationScoped — it would silently drop from the admin's "
                                + "@All List<AdminDataProvider>."));
            }
        }
        if (!problems.isEmpty()) {
            errors.produce(new ValidationErrorBuildItem(problems));
        }
    }
}
