import ctypes,os,subprocess,sys
class Data(ctypes.Union):_fields_=[('b',ctypes.c_char*20),('s',ctypes.c_short*10),('l',ctypes.c_long*5)]
class Message(ctypes.Structure):_fields_=[('type',ctypes.c_int),('serial',ctypes.c_ulong),('send_event',ctypes.c_int),('display',ctypes.c_void_p),('window',ctypes.c_ulong),('message_type',ctypes.c_ulong),('format',ctypes.c_int),('data',Data)]
class Event(ctypes.Union):_fields_=[('message',Message),('pad',ctypes.c_long*24)]
x=ctypes.CDLL('libX11.so.6');x.XOpenDisplay.argtypes=[ctypes.c_char_p];x.XOpenDisplay.restype=ctypes.c_void_p;x.XInternAtom.argtypes=[ctypes.c_void_p,ctypes.c_char_p,ctypes.c_int];x.XInternAtom.restype=ctypes.c_ulong;x.XSendEvent.argtypes=[ctypes.c_void_p,ctypes.c_ulong,ctypes.c_int,ctypes.c_long,ctypes.POINTER(Event)];x.XFlush.argtypes=[ctypes.c_void_p];x.XCloseDisplay.argtypes=[ctypes.c_void_p]
d=x.XOpenDisplay(os.environ['DISPLAY'].encode());assert d
windows=subprocess.check_output(['xdotool','search','--all','--onlyvisible','--pid',sys.argv[1]],text=True).splitlines();assert windows
try:
 for window in windows:
  protocols=subprocess.check_output(['xprop','-id',window,'WM_PROTOCOLS'],text=True)
  if 'WM_DELETE_WINDOW' not in protocols:continue
  e=Event();e.message.type=33;e.message.display=d;e.message.window=int(window);e.message.message_type=x.XInternAtom(d,b'WM_PROTOCOLS',False);e.message.format=32;e.message.data.l[0]=x.XInternAtom(d,b'WM_DELETE_WINDOW',False);assert x.XSendEvent(d,int(window),False,0,ctypes.byref(e));x.XFlush(d)
finally:x.XCloseDisplay(d)
