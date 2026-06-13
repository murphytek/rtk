//! Intra-stack-trace frame filtering.
//!
//! Collapses runs of framework noise frames (Groovy internals, reflection
//! wrappers, test runners) within a single trace into a compact marker.
//! Runs of fewer than 3 noise frames pass through unchanged.

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    static ref NOISE_PATTERNS: Vec<Regex> = vec![
        Regex::new(r"^\s+at groovyjarjarasm\.").unwrap(),
        Regex::new(r"^\s+at org\.codehaus\.groovy\.").unwrap(),
        Regex::new(r"^\s+at sun\.reflect\.").unwrap(),
        Regex::new(r"^\s+at jdk\.internal\.reflect\.").unwrap(),
        Regex::new(r"^\s+at jdk\.proxy\d+\.").unwrap(),
        Regex::new(r"^\s+at java\.lang\.reflect\.Method\.invoke").unwrap(),
        Regex::new(r"^\s+at org\.junit\.platform\.engine\.support\.hierarchical\.").unwrap(),
        Regex::new(r"^\s+at org\.junit\.runners\.").unwrap(),
        Regex::new(r"^\s+at org\.junit\.internal\.runners\.").unwrap(),
        Regex::new(r"^\s+at org\.gradle\.api\.internal\.tasks\.testing\.").unwrap(),
        Regex::new(r"^\s+at org\.gradle\.internal\.dispatch\.").unwrap(),
        Regex::new(r"^\s+at org\.gradle\.process\.internal\.worker\.").unwrap(),
        Regex::new(r"^\s+at org\.apache\.maven\.surefire\.").unwrap(),
        Regex::new(r"^\s+at org\.apache\.maven\.plugin\.surefire\.").unwrap(),
        Regex::new(r"^\s+at worker\.org\.gradle\.process\.internal\.").unwrap(),
    ];

    static ref EXCEPTION_HEADER: Regex = Regex::new(
        r"^(?:[A-Za-z_][\w$]*\.)+[A-Za-z_][\w$]*(?:Exception|Error|Throwable)(?::.*)?$"
    ).unwrap();

    static ref CAUSED_BY: Regex = Regex::new(r"^Caused by:\s").unwrap();

    static ref AT_FRAME: Regex = Regex::new(r"^\s+at\s").unwrap();

    static ref MORE_FRAMES: Regex = Regex::new(r"^\s+\.\.\.\s+\d+\s+more\s*$").unwrap();
}

fn is_noise(line: &str) -> bool {
    NOISE_PATTERNS.iter().any(|re| re.is_match(line))
}

fn is_trace_header(line: &str) -> bool {
    let t = line.trim_start();
    EXCEPTION_HEADER.is_match(t) || CAUSED_BY.is_match(t)
}

fn is_frame(line: &str) -> bool {
    AT_FRAME.is_match(line) || MORE_FRAMES.is_match(line)
}

/// Trim noise frames from a single trace segment (header + frames).
/// Returns the processed lines.
fn trim_trace(lines: &[&str]) -> Vec<String> {
    if lines.is_empty() {
        return Vec::new();
    }

    // lines[0] is the header; always kept.
    let header = lines[0];
    let frames = &lines[1..];

    if frames.is_empty() {
        return vec![header.to_string()];
    }

    let last_idx = frames.len() - 1;
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    out.push(header.to_string());

    // Walk frames, collecting contiguous noise runs.
    // Within each run: the first frame is always kept (shows the noise boundary),
    // and the remainder is collapsed to a marker if len >= 3.
    let mut noise_run: Vec<usize> = Vec::new(); // indices into `frames` that are noise

    let flush_noise = |noise_run: &mut Vec<usize>, out: &mut Vec<String>, frames: &[&str]| {
        if noise_run.is_empty() {
            return;
        }
        // Always emit the first frame of the run.
        out.push(frames[noise_run[0]].to_string());
        let tail = &noise_run[1..];
        if tail.len() >= 3 {
            out.push(format!("(... {} frames omitted ...)", tail.len()));
        } else {
            for &idx in tail {
                out.push(frames[idx].to_string());
            }
        }
        noise_run.clear();
    };

    for (i, &frame) in frames.iter().enumerate() {
        let keep_forced = i == 0 || i == last_idx;
        if keep_forced || !is_noise(frame) {
            flush_noise(&mut noise_run, &mut out, frames);
            out.push(frame.to_string());
        } else {
            noise_run.push(i);
        }
    }
    // Trailing noise run (shouldn't normally happen since last frame is forced,
    // but flush defensively).
    flush_noise(&mut noise_run, &mut out, frames);

    out
}

/// Remove framework noise frames from stack traces in `input`.
///
/// Plain text passes through unchanged. Contiguous runs of ≥3 noise frames
/// within a trace are replaced with `(... N frames omitted ...)`.
pub fn trim_stack_noise(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }

    let raw_lines: Vec<&str> = input.split('\n').collect();
    let has_trailing_newline = matches!(raw_lines.last(), Some(&""));
    let slice_end = if has_trailing_newline {
        raw_lines.len() - 1
    } else {
        raw_lines.len()
    };
    let lines = &raw_lines[..slice_end];

    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        if is_trace_header(line) {
            // Consume header + all continuation frames.
            let start = i;
            i += 1;
            while i < lines.len() && (is_frame(lines[i]) || is_trace_header(lines[i])) {
                // A Caused-by inside a trace: absorb as a new sub-trace.
                // We'll re-parse the whole segment split by Caused-by headers.
                i += 1;
            }
            let segment = &lines[start..i];
            // Split segment by Caused-by / exception headers so each level
            // is trimmed independently.
            let mut seg_start = 0;
            while seg_start < segment.len() {
                // Find the next sub-header after seg_start+1.
                let mut seg_end = seg_start + 1;
                while seg_end < segment.len() && !is_trace_header(segment[seg_end]) {
                    seg_end += 1;
                }
                let sub = &segment[seg_start..seg_end];
                for trimmed_line in trim_trace(sub) {
                    out.push(trimmed_line);
                }
                seg_start = seg_end;
            }
        } else {
            out.push(line.to_string());
            i += 1;
        }
    }

    let mut joined = out.join("\n");
    if has_trailing_newline {
        joined.push('\n');
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_groovy_frames_collapse_user_frame_preserved() {
        let input = "\
java.lang.RuntimeException: Compile failed
    at com.example.MyTask.run(MyTask.java:42)
    at groovyjarjarasm.asm.ClassReader.<init>(ClassReader.java:200)
    at groovyjarjarasm.asm.ClassReader.accept(ClassReader.java:425)
    at groovyjarjarasm.asm.ClassReader.readClass(ClassReader.java:600)
    at org.codehaus.groovy.ast.ClassNode.<init>(ClassNode.java:85)
    at org.codehaus.groovy.ast.ClassNode.parse(ClassNode.java:120)
    at sun.reflect.GeneratedMethodAccessor47.invoke(Unknown Source)
    at sun.reflect.DelegatingMethodAccessorImpl.invoke(DelegatingMethodAccessorImpl.java:43)
    at java.lang.reflect.Method.invoke(Method.java:498)
    at org.junit.platform.engine.support.hierarchical.NodeTestTask.execute(NodeTestTask.java:1)
    at org.gradle.api.internal.tasks.testing.junit.JUnitTestClassExecutor.execute(JUnitTestClassExecutor.java:1)
    at com.example.Main.main(Main.java:12)
";
        let out = trim_stack_noise(input);
        // Header kept
        assert!(out.contains("java.lang.RuntimeException: Compile failed"));
        // First frame kept (even though it's user code here)
        assert!(out.contains("at com.example.MyTask.run(MyTask.java:42)"));
        // The first noise frame (index 1 in frames = index 0 in noise) is kept
        // because it's the second frame overall (first after header).
        assert!(out.contains("at groovyjarjarasm.asm.ClassReader.<init>"));
        // Omission marker present
        assert!(out.contains("frames omitted"), "expected omit marker:\n{}", out);
        // Last frame kept
        assert!(out.contains("at com.example.Main.main(Main.java:12)"));
        // No middle noise frames leaked through
        assert!(!out.contains("at sun.reflect.DelegatingMethodAccessorImpl"));
    }

    #[test]
    fn two_noise_frames_pass_through_unchanged() {
        let input = "\
java.lang.RuntimeException: Err
    at com.example.Foo.bar(Foo.java:1)
    at sun.reflect.GeneratedMethodAccessor47.invoke(Unknown Source)
    at sun.reflect.DelegatingMethodAccessorImpl.invoke(DelegatingMethodAccessorImpl.java:43)
    at com.example.Foo.main(Foo.java:99)
";
        let out = trim_stack_noise(input);
        assert!(!out.contains("frames omitted"), "should not collapse 2 noise frames:\n{}", out);
        assert!(out.contains("at sun.reflect.GeneratedMethodAccessor47.invoke"));
        assert!(out.contains("at sun.reflect.DelegatingMethodAccessorImpl.invoke"));
    }

    #[test]
    fn zero_noise_frames_unchanged() {
        let input = "\
java.lang.RuntimeException: Clean
    at com.example.A.a(A.java:1)
    at com.example.B.b(B.java:2)
    at com.example.C.c(C.java:3)
";
        let out = trim_stack_noise(input);
        assert_eq!(out, input);
    }

    #[test]
    fn caused_by_chain_each_level_processed() {
        let input = "\
java.lang.RuntimeException: Outer
    at com.example.Outer.run(Outer.java:10)
    at groovyjarjarasm.asm.ClassReader.<init>(ClassReader.java:200)
    at groovyjarjarasm.asm.ClassReader.accept(ClassReader.java:425)
    at groovyjarjarasm.asm.ClassReader.readClass(ClassReader.java:600)
    at org.codehaus.groovy.ast.ClassNode.<init>(ClassNode.java:85)
    at com.example.Outer.end(Outer.java:99)
Caused by: java.io.IOException: Middle
    at com.example.Middle.call(Middle.java:5)
    at org.codehaus.groovy.ast.ClassNode.<init>(ClassNode.java:85)
    at org.codehaus.groovy.ast.ClassNode.parse(ClassNode.java:120)
    at sun.reflect.GeneratedMethodAccessor47.invoke(Unknown Source)
    at sun.reflect.DelegatingMethodAccessorImpl.invoke(DelegatingMethodAccessorImpl.java:43)
    at com.example.Middle.end(Middle.java:50)
Caused by: java.lang.IllegalStateException: Inner
    at com.example.Inner.go(Inner.java:1)
    at com.example.Inner.done(Inner.java:99)
";
        let out = trim_stack_noise(input);
        // All three headers present
        assert!(out.contains("java.lang.RuntimeException: Outer"));
        assert!(out.contains("Caused by: java.io.IOException: Middle"));
        assert!(out.contains("Caused by: java.lang.IllegalStateException: Inner"));
        // Noise collapsed in outer and middle levels
        assert!(out.contains("frames omitted"));
        // Inner level has no noise — both frames intact
        assert!(out.contains("at com.example.Inner.go(Inner.java:1)"));
        assert!(out.contains("at com.example.Inner.done(Inner.java:99)"));
    }

    #[test]
    fn plain_text_passes_through() {
        let input = "[INFO] Building foo 1.0\n[INFO] BUILD SUCCESS\nTotal time: 5s\n";
        let out = trim_stack_noise(input);
        assert_eq!(out, input);
    }
}
