package archrules.deployment;

import java.util.List;

import admin.adminapi.AdminDataProvider;
import admin.adminapi.SectionData;
import jakarta.enterprise.context.Dependent;

/**
 * A DELIBERATELY-WRONG AdminDataProvider: it implements the real contract but is @Dependent, not
 * @ApplicationScoped — the exact mistake {@link ArchRulesProcessor#validateAdminProviders} exists to
 * catch (a @Dependent provider silently drops from the admin's {@code @All List<AdminDataProvider>}).
 * ArC itself is perfectly happy with a @Dependent bean implementing an interface, so if augmentation
 * rejects this app it can ONLY be because our build step ran — that is what the negative-fixture test
 * proves.
 */
@Dependent
public class BadAdminProvider implements AdminDataProvider {

    @Override
    public String getId() {
        return "bad";
    }

    @Override
    public String getSection() {
        return "Bad";
    }

    @Override
    public String getLabel() {
        return "Deliberately @Dependent";
    }

    @Override
    public SectionData data() {
        return new SectionData(List.of(), null);
    }
}
