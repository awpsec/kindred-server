import gi, json, os
from pathlib import Path
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk, GLib
name=os.environ['FIXTURE_NAME']; root=Path(os.environ['FIXTURE_ROOT'])
GLib.set_prgname('kindred-agent-fixture')
window=Gtk.Window(title=name); window.set_default_size(600,350)
box=Gtk.Box(orientation=Gtk.Orientation.VERTICAL,spacing=12);window.add(box)
entry=Gtk.Entry();entry.get_accessible().set_name('Draft');box.pack_start(entry,False,False,0)
counter=Gtk.Label(label='Submissions: 0');box.pack_start(counter,False,False,0)
button=Gtk.Button(label='Submit once');box.pack_start(button,False,False,0)
check=Gtk.CheckButton(label='Enabled');box.pack_start(check,False,False,0)
state={'name':name,'count':0,'text':'','enabled':False}
def save(*_):
    state.update(text=entry.get_text(),enabled=check.get_active());(root/'state.json').write_text(json.dumps(state))
def submit(*_):
    state['count']+=1;counter.set_text('Submissions: '+str(state['count']));button.set_label('Submitted');save()
entry.connect('changed',save);check.connect('toggled',save);button.connect('clicked',submit)
window.connect('destroy',Gtk.main_quit);window.show_all();save();Gtk.main()
