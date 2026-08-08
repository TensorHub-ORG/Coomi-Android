package app.coomi;

import org.junit.Test;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

public class RootAccessControllerTest {

    @Test
    public void recognizesUidZeroFromIdOutput() {
        assertTrue(RootAccessController.hasRootIdentity(
            "uid=0(root) gid=0(root) groups=0(root)"));
    }

    @Test
    public void rejectsShellIdentity() {
        assertFalse(RootAccessController.hasRootIdentity(
            "uid=2000(shell) gid=2000(shell) groups=2000(shell)"));
    }

    @Test
    public void rejectsNullOrUnrelatedOutput() {
        assertFalse(RootAccessController.hasRootIdentity(null));
        assertFalse(RootAccessController.hasRootIdentity("permission denied"));
    }
}
