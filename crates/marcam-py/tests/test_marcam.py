"""Tests for the installed `marcam` wheel (plain asserts; run with python)."""
from pathlib import Path

import marcam

ROOT = Path(__file__).resolve().parents[3]


def test_example_matches_golden():
    job = marcam.example_job()
    res = marcam.plan_and_post(job, max_points=1000)
    lines = [p["lines"] for p in res["programs"]]
    assert lines == [21787, 15146], lines
    for p in res["programs"]:
        assert p["issues"] == [], p["issues"]
        golden = (ROOT / "tests" / "golden" / f"{p['name']}.nc").read_text().replace("\r\n", "\n")
        assert p["gcode"] == golden, p["name"]


def test_linuxcnc_and_errors():
    res = marcam.plan_and_post(marcam.example_job(), machine="linuxcnc")
    assert "G93" in res["programs"][0]["gcode"]
    assert marcam.machine("linuxcnc")["feed_mode"] == "inverse_time"
    for bad in ("{", {"schema_version": "0.1"}):
        try:
            marcam.plan_and_post(bad)
        except ValueError:
            pass
        else:
            raise AssertionError("expected ValueError")


def test_mesh_job_and_simulation():
    job_path = ROOT / "examples" / "knob" / "knob_finish.job.json"
    stl = ROOT / "examples" / "knob" / "knob.stl"
    job = job_path.read_text()
    res = marcam.plan_and_post(job, mesh=str(stl), max_points=1000)
    assert [p["name"] for p in res["programs"]] == [
        "knob_3_mesh_rough",
        "knob_4_finish_along",
        "knob_5_finish_around",
    ]
    for p in res["programs"]:
        assert p["issues"] == []
        assert p["gouge"]["max_depth"] < 0.01
    # bytes work too
    res2 = marcam.plan_and_post(job, mesh=stl.read_bytes(), max_points=1000)
    assert res2["programs"][0]["gcode"] == res["programs"][0]["gcode"]
    sim = marcam.simulate(marcam.example_job(), dx=0.2, na=90)
    assert sim["stocks"][0]["rapid_hits"] == 0
    assert sim["stocks"][0]["gouge_cells"] == 0



def test_time_estimates():
    res = marcam.plan_and_post(marcam.example_job(), max_points=1000)
    total = sum(p["time"]["total_s"] for p in res["programs"])
    assert abs(res["time_total_s"] - total) < 1e-6 and total > 60
    for p in res["programs"]:
        ops = sum(o["seconds"] for o in p["time"]["ops"])
        assert ops <= p["time"]["total_s"] + 1e-6
    # any G-code: 100 mm at 600 mm/min on the Z1 is a little over 10 s with acceleration
    e = marcam.estimate("G0 X0 Y0 Z5\nG1 X100 F600\n")
    assert e["machine"] == "makera-z1-4axis" and 10.0 < e["feed_s"] < 10.1, e


def test_picking():
    fx = ROOT / "tests" / "fixtures" / "picking"
    project = (fx / "bracket.project.json").read_text()
    stl = (fx / "bracket.stl").read_bytes()
    pk = marcam.pickables(project, stl)
    holes = [x for x in pk["pickables"] if x["kind"] == "hole"]
    assert len(holes) == 5
    pocket = next(x for x in pk["pickables"] if x["kind"] == "pocket")
    res = marcam.apply_picks(project, "pocket", [pocket["id"]], stl, tool=2)
    assert res["params"]["depth"] == 5.0 and len(res["params"]["paths"]) == 2
    # the generic call reaches it too
    assert marcam.call("pickables", project, {}, stl)["setup"] == "vise"


def test_toml_projects():
    # a TOML project (what marcam writes) plans like its JSON; to_toml writes one
    text = (ROOT / "examples" / "knob" / "knob.marcam.toml").read_text()
    a = marcam.plan_and_post(text, max_points=1000)
    t = marcam.to_toml(marcam.example_job())
    assert t.startswith("# marcam project") and "[[setups.operations]]" in t
    b = marcam.plan_and_post(t, max_points=1000)
    assert [p["gcode"] for p in a["programs"]] == [p["gcode"] for p in b["programs"]]


def test_replace_model():
    import json
    fx = ROOT / "tests" / "fixtures" / "picking"
    project = json.loads((fx / "oval_grip.project.json").read_text())
    grip = (fx / "oval_grip.stl").read_bytes()
    pocket = next(x for x in marcam.pickables(project, grip)["pickables"] if x["kind"] == "pocket")
    project["setups"][0]["operations"] = [{"id": "op1", "type": "pocket", "tool": 2, "rect": [1, 1, 5, 5], "depth": 1, "step_down": 0.5,
                                           "stepover": 0.5, "feed": 800, "plunge_feed": 200, "picks": [pocket["signature"]]}]
    project["setups"][0]["outputs"] = [{"name": "grip", "title": "grip", "operations": ["op1"]}]
    # the bracket has no such pocket: the operation is in error
    res = marcam.replace_model(project, (fx / "bracket.stl").read_bytes(), "bracket.stl")
    assert res["operations"][0]["ok"] is False and "Pocket" in res["operations"][0]["error"]
    assert res["project"]["models"][0]["file"] == "bracket.stl"
    assert "error" in res["project"]["setups"][0]["operations"][0]
    # the grip again: found, the error cleared
    res = marcam.replace_model(res["project"], grip, "oval_grip.stl")
    assert res["operations"][0]["ok"] is True and "error" not in res["project"]["setups"][0]["operations"][0]


def test_custom_toolpath():
    # G-code from a Python script (the use the custom toolpath is for): a slot on the vise,
    # added to a project, planned and posted like marcam's own operations
    import json
    fx = ROOT / "tests" / "fixtures" / "picking"
    project = json.loads((fx / "oval_grip.project.json").read_text())
    lines = ["G21 G90", "G0 X5 Y5 Z5", "G1 Z-0.5 F50"]
    lines += [f"G1 X{5 + k} Y5 F300" for k in range(1, 11)]
    lines += ["G0 Z5"]
    res = marcam.add_custom_toolpath(project, project["setups"][0]["id"], "\n".join(lines), name="script slot")
    assert res["read"]["errors"] == [] and res["read"]["feed_moves"] == 11, res["read"]
    op = next(o for o in res["project"]["setups"][0]["operations"] if o["id"] == res["operation"])
    assert op["type"] == "custom" and op["name"] == "script slot"
    # off the 4th axis frame's rules: a vise takes no A
    bad = marcam.add_custom_toolpath(project, project["setups"][0]["id"], "G0 X0 Y0 Z5 A90\nG1 Z-1 F50")
    assert "no rotary axis" in bad["read"]["errors"][0]


if __name__ == "__main__":
    for name, fn in list(globals().items()):
        if name.startswith("test_"):
            fn()
            print("ok", name)
    print("python OK", marcam.version())

