"""Apply only the executor fixture seam to an already isolated exact-baseline clone.
No behavior correction; baseline guest screenshot/input logic stays byte-identical.
"""
import pathlib, subprocess, sys
source=pathlib.Path(__file__).resolve().parents[1]
target=pathlib.Path(sys.argv[1]).resolve()
allowed=pathlib.Path('/root/.openrig/workspace/projects/kindred/checkouts/dev').resolve()
assert target.is_relative_to(allowed) and target!=source, 'Use separate dev baseline clone'
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=target,text=True).strip()=='51a4d5a578f72ea745b02fa3b5b962675197ee01'
assert not subprocess.check_output(['git','status','--porcelain'],cwd=target,text=True).strip(), 'Baseline clone must start clean'
p=target/'src/guest.rs';s=p.read_text()
s=s.replace('    let display = format!(":{screen}");\n    if tool.starts_with','    if tool.starts_with',1)
s=s.replace('    let browser = if screen == 1 {','    execute_display(tool, args, screen).await\n}\nasync fn execute_display(tool: &str, args: &Value, screen: i64) -> Result<Value> {\n    let display = format!(":{screen}");\n    let browser = if screen == 1 {',1)
current=(source/'src/guest.rs').read_text();a=current.index('    #[tokio::test]\n    #[ignore = "explicit disposable');b=current.index('    #[test]\n    fn keyboard_aliases',a)
s=s.replace('    #[test]\n    fn keyboard_aliases',current[a:b]+'    #[test]\n    fn keyboard_aliases',1);p.write_text(s)
for name in ['run-computer-executor-fixture.py','test-computer-executor.cjs']:
    (target/'tools'/name).write_bytes((source/'tools'/name).read_bytes())
print('Applied test-only direct X11 fixture seam; baseline behavior unchanged')
