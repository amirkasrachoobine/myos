package org.myos.sdk;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.DirectoryStream;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.nio.file.StandardOpenOption;
import java.util.ArrayList;
import java.util.List;
import java.util.Optional;

/**
 * The app's own files, in its data directory
 * (/var/lib/myos/apps/&lt;id&gt;/data). Every app has one, owned by its own
 * user and closed to every other app; it survives reboots and upgrades and
 * is deleted by {@code jrun remove} (unless --keep-data).
 *
 * Names are relative paths inside that directory; "..", absolute paths and
 * symlinks out of it are refused. For anything else, plain java.nio works
 * too, with the app's own (non-root) permissions.
 */
public final class Files {
    private final Path root;

    Files(Path root) {
        this.root = root.toAbsolutePath().normalize();
    }

    /** The data directory itself. */
    public Path root() {
        return root;
    }

    /** A path inside the data directory. */
    public Path path(String name) {
        Path p = root.resolve(name).normalize();
        if (name.isEmpty() || Path.of(name).isAbsolute() || !p.startsWith(root) || p.equals(root)) {
            throw new IllegalArgumentException("\"" + name + "\" isn't a file name inside the app's data directory");
        }
        try {
            // Follow symlinks for what exists; it must still be inside.
            Path real = p;
            while (real != null && !java.nio.file.Files.exists(real, java.nio.file.LinkOption.NOFOLLOW_LINKS)) {
                real = real.getParent();
            }
            if (real != null && !real.toRealPath().startsWith(root.toRealPath())) {
                throw new IllegalArgumentException("\"" + name + "\" leads out of the app's data directory");
            }
        } catch (IOException e) {
            // let the actual operation report it
        }
        return p;
    }

    public boolean exists(String name) {
        return java.nio.file.Files.exists(path(name));
    }

    public Optional<String> read(String name) throws IOException {
        Path p = path(name);
        if (!java.nio.file.Files.exists(p)) return Optional.empty();
        return Optional.of(java.nio.file.Files.readString(p, StandardCharsets.UTF_8));
    }

    public byte[] readBytes(String name) throws IOException {
        return java.nio.file.Files.readAllBytes(path(name));
    }

    /** Replaces the file atomically: readers see the old or the new contents, never half. */
    public void write(String name, String text) throws IOException {
        writeBytes(name, text.getBytes(StandardCharsets.UTF_8));
    }

    public void writeBytes(String name, byte[] data) throws IOException {
        Path p = path(name);
        java.nio.file.Files.createDirectories(p.getParent());
        Path tmp = p.resolveSibling("." + p.getFileName() + ".tmp");
        java.nio.file.Files.write(tmp, data);
        try {
            java.nio.file.Files.move(tmp, p, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
        } catch (AtomicMoveNotSupportedException e) {
            java.nio.file.Files.move(tmp, p, StandardCopyOption.REPLACE_EXISTING);
        }
    }

    public void append(String name, String text) throws IOException {
        Path p = path(name);
        java.nio.file.Files.createDirectories(p.getParent());
        java.nio.file.Files.writeString(p, text, StandardCharsets.UTF_8, StandardOpenOption.CREATE,
                StandardOpenOption.APPEND);
    }

    public boolean delete(String name) throws IOException {
        return java.nio.file.Files.deleteIfExists(path(name));
    }

    /** Names in the data directory (or a directory inside it), sorted. */
    public List<String> list(String dir) throws IOException {
        Path d = dir.isEmpty() || dir.equals(".") ? root : path(dir);
        List<String> out = new ArrayList<>();
        try (DirectoryStream<Path> s = java.nio.file.Files.newDirectoryStream(d)) {
            for (Path p : s) out.add(p.getFileName().toString());
        }
        out.sort(null);
        return out;
    }

    public List<String> list() throws IOException {
        return list("");
    }
}
