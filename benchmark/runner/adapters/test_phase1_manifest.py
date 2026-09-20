import importlib.util
from pathlib import Path
P=Path(__file__).with_name("phase1_manifest.py")
spec=importlib.util.spec_from_file_location("phase1_manifest",P); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
def test_deterministic_disjoint_split():
 ids=[f"task-{i}" for i in range(200)]
 dev=[x for x in ids if m.bucket("swebench-verified",x) in (0,1)]
 held=[x for x in ids if m.bucket("swebench-verified",x) not in (0,1)]
 assert not set(dev)&set(held)
 assert sorted(dev+held)==sorted(ids)
 assert [m.bucket("swebench-verified",x) for x in ids]==[m.bucket("swebench-verified",x) for x in ids]
def test_suite_names_salt_the_split():
 assert any(m.bucket("swebench-verified",f"t{i}") != m.bucket("deepswe-v1.1",f"t{i}") for i in range(20))
