#![allow(dead_code)]
#[path = "/home/dawids/Projects/mmo/crates/worldgen/src/noise.rs"] mod noise;
#[path = "/home/dawids/Projects/mmo/crates/worldgen/src/spline.rs"] mod spline;
use noise::{OctaveNoise, PerlinNoise}; use spline::Spline;
use std::io::Write;

fn splitmix(s:&mut u64)->u64{*s=s.wrapping_add(0x9E37_79B9_7F4A_7C15);let mut z=*s;z=(z^(z>>30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);z=(z^(z>>27)).wrapping_mul(0x94D0_49BB_1331_11EB);z^(z>>31)}

/// Minecraft NormalNoise-style stack: octave i weighted amp_i * 2^-i (persistence),
/// two decorrelated stacks summed, scaled by the vanilla value factor.
struct McNoise{ a:Vec<PerlinNoise>, b:Vec<PerlinNoise>, w:Vec<f64>, f:Vec<f64>, vf:f64 }
impl McNoise{
  fn new(seed:u64, first:i32, amps:&[f64])->Self{
    let mut s=seed; let n=amps.len();
    let a=(0..n).map(|_|PerlinNoise::new(splitmix(&mut s))).collect();
    let b=(0..n).map(|_|PerlinNoise::new(splitmix(&mut s))).collect();
    let low=2f64.powi(n as i32-1)/(2f64.powi(n as i32)-1.0);
    let w=(0..n).map(|i|amps[i]*low/2f64.powi(i as i32)).collect();
    let f=(0..n).map(|i|2f64.powi(first+i as i32)).collect();
    let vf=(1.0/6.0)/(0.1*(1.0+1.0/n as f64));
    Self{a,b,w,f,vf}}
  fn s(&self,x:f64,z:f64)->f64{ let mut acc=0.0; const K:f64=1.0181268882175227;
    for i in 0..self.w.len(){ let f=self.f[i]; acc+=self.w[i]*(self.a[i].sample2(x*f,z*f)+self.b[i].sample2(x*f*K,z*f*K)); }
    acc*self.vf }
}
fn c(v:f64)->Spline{Spline::constant(v)}
fn sp(pts:&[(f64,f64,f64)])->Spline{Spline::Curve{points:pts.iter().map(|&(l,v,d)|Spline::point(l,c(v),d)).collect()}}

struct World{ variant:u32, oc:OctaveNoise, oe:OctaveNoise, or:OctaveNoise, mc:McNoise, me:McNoise, mr:McNoise, mw1:McNoise, mw2:McNoise,
  cc:Spline, ob:Spline, em:Spline, fs:Spline, jg:Spline, // current
  raw2:Spline, base2:Spline, mgate:Spline, egate:Spline, pvs:Spline, fac2:Spline }
const SX:f64=100_000.0; const SZ:f64=50_000.0;
impl World{
  fn new(variant:u32)->Self{ let seed=0x00D5_C0DE_8D78u64;
    let ca=[1.75,1.0,2.0,3.0,2.0,2.0,1.0]; let ea=[2.0,1.75,1.5,1.5,1.3,1.0]; let ra=[1.0,2.0,1.0];
    World{variant,
      oc:OctaveNoise::new(seed^0xC0,-10,ca.to_vec()), oe:OctaveNoise::new(seed^0xE0,-10,ea.to_vec()), or:OctaveNoise::new(seed^0xD0,-8,ra.to_vec()),
      mc:McNoise::new(seed^0xC0,-10,&ca), me:McNoise::new(seed^0xE0,-10,&ea), mr:McNoise::new(seed^0xD0,-8,&ra),
      mw1:McNoise::new(seed^0xA1,-9,&[1.0,0.5]), mw2:McNoise::new(seed^0xA2,-9,&[1.0,0.5]),
      cc:sp(&[(0.0,0.0,0.0),(0.4,0.6,1.0),(1.0,1.2,0.0)]),
      ob:sp(&[(-1.0,-0.9,0.0),(-0.2,-0.3,0.5),(0.0,0.02,0.6),(0.6,0.9,1.0),(1.2,1.8,0.5)]),
      em:sp(&[(-1.0,1.0,0.0),(0.2,0.5,0.0),(1.0,0.15,0.0)]),
      fs:sp(&[(-1.0,0.6,0.0),(0.0,1.2,0.0),(1.0,3.0,0.0)]),
      jg:sp(&[(0.0,0.0,0.0),(1.0,0.6,0.0)]),
      // V2+: |cont| -> raw continentalness (-1 deep ocean .. 0 coast .. 1.2 deep interior)
      raw2:sp(&[(0.0,-1.0,0.0),(0.35,-0.5,1.6),(0.55,0.0,2.2),(0.9,0.5,1.2),(1.5,1.0,0.6),(2.2,1.2,0.0)]),
      // raw -> base elevation in offset units (x96 m): shelf, coast, plains, interior plateau
      base2:sp(&[(-1.0,-0.62,0.0),(-0.5,-0.42,0.4),(-0.12,-0.12,1.0),(0.0,0.015,0.9),(0.12,0.07,0.3),(0.5,0.22,0.4),(1.0,0.5,0.5),(1.2,0.6,0.0)]),
      mgate:sp(&[(0.1,0.0,0.0),(0.35,0.35,1.8),(0.7,1.0,0.0)]),          // mountains only inland
      egate:sp(&[(-0.9,1.0,0.0),(-0.35,0.75,-0.8),(0.1,0.2,-0.8),(0.6,0.03,0.0)]), // low erosion = tall
      pvs:sp(&[(-1.0,0.0,0.0),(-0.3,0.12,0.4),(0.4,0.55,0.9),(1.0,1.0,0.6)]),   // valleys..peaks
      fac2:sp(&[(-1.0,0.6,0.0),(0.0,1.2,0.0),(1.0,3.0,0.0)]) } }
  /// surface height in meters
  fn h(&self,x:f64,z:f64)->f64{ let (x,z)=(x+SX,z+SZ);
    let h = match self.variant{
      0|1=>{ let (a,e,r)= if self.variant==0 {(self.oc.sample2(x*0.13,z*0.13),self.oe.sample2(x*0.25,z*0.25),self.or.sample2(x*0.5,z*0.5))}
                          else {(self.mc.s(x*0.13,z*0.13),self.me.s(x*0.25,z*0.25),self.mr.s(x*0.5,z*0.5))};
        let raw=-0.5+self.cc.eval(a.abs()); let off=(self.ob.eval(raw)*self.em.eval(e)).clamp(-1.0,2.0);
        64.0+off*96.0+32.0*self.jg.eval(off)*r.abs()/self.fs.eval(off) }
      _=>{ let cs:f64=std::env::var("CS").ok().and_then(|v|v.parse().ok()).unwrap_or(1.0); let a=self.mc.s(x*0.13/cs,z*0.13/cs); let e=self.me.s(x*0.25/cs,z*0.25/cs);
        let (rx,rz)= if self.variant>=3 { // domain warp: bend ridge blobs into long ranges
            (x+self.mw1.s(x*0.35,z*0.35)*1400.0, z+self.mw2.s(x*0.35,z*0.35)*1400.0)} else {(x,z)};
        let r=self.mr.s(rx*0.22,rz*0.22);
        let pv=-3.0*((r.abs()-0.6666667).abs()-0.3333333);
        let raw=self.raw2.eval(a.abs());
        let base=self.base2.eval(raw);
        let eg=self.egate.eval(e);
        let mount=self.mgate.eval(raw)*eg*self.pvs.eval(pv.clamp(-1.0,1.0));
        let off=(base*(0.55+0.45*eg) + mount*1.75).clamp(-1.0,2.0);
        let base_only = if base<0.0 {base} else {off};
        64.0+base_only*96.0 } };
    h.clamp(-64.0,320.0) }
}
fn lerp3(a:[f64;3],b:[f64;3],t:f64)->[f64;3]{let t=t.clamp(0.0,1.0);[a[0]+(b[0]-a[0])*t,a[1]+(b[1]-a[1])*t,a[2]+(b[2]-a[2])*t]}
fn ramp(st:&[(f64,[f64;3])],v:f64)->[f64;3]{let mut p=st[0];for &s in st{if v<=s.0{return lerp3(p.1,s.1,(v-p.0)/(s.0-p.0).max(1e-9));}p=s;}p.1}
fn color(h:f64)->[f64;3]{ if h<63.5 {return ramp(&[(0.0,[110.,190.,215.]),(8.0,[60.,140.,200.]),(40.0,[30.,80.,160.]),(128.0,[12.,30.,90.])],64.0-h);}
  if h<65.5 {return [222.,205.,150.];}
  ramp(&[(0.0,[96.,168.,84.]),(30.0,[70.,140.,66.]),(80.0,[120.,130.,80.]),(140.0,[140.,120.,100.]),(200.0,[170.,165.,160.]),(250.0,[245.,245.,250.])],h-64.0)}
fn main(){ let a:Vec<String>=std::env::args().collect(); let variant:u32=a[1].parse().unwrap(); let span:f64=a[2].parse().unwrap(); let n:usize=a[3].parse().unwrap(); let out=&a[4];
  let w=World::new(variant); let step=span/n as f64; let (x0,z0)=(8.5-span/2.0,8.5-span/2.0);
  let mut hs=vec![0f32;n*n];
  std::thread::scope(|s|{ for (band,rows) in hs.chunks_mut(n*64).enumerate(){ let w=&w; s.spawn(move||{ for (r,row) in rows.chunks_mut(n).enumerate(){ let z=z0+((band*64+r) as f64+0.5)*step; for (cx,h) in row.iter_mut().enumerate(){ *h=w.h(x0+(cx as f64+0.5)*step,z) as f32; }}});}});
  let at=|x:usize,z:usize|hs[z*n+x] as f64; let sun=[-0.5f64,0.7071,-0.5];
  let mut buf=Vec::with_capacity(n*n*3);
  for z in 0..n{for x in 0..n{ let h=at(x,z); let (xl,xr,zu,zd)=(x.saturating_sub(1),(x+1).min(n-1),z.saturating_sub(1),(z+1).min(n-1));
    let dx=(at(xr,z)-at(xl,z))/((xr-xl) as f64*step); let dz=(at(x,zd)-at(x,zu))/((zd-zu) as f64*step);
    let shade= if h<63.5 {1.0} else { let nr=[-dx*2.0,1.0,-dz*2.0]; let l=(nr[0]*nr[0]+1.0+nr[2]*nr[2]).sqrt(); let lam=(nr[0]*sun[0]+sun[1]+nr[2]*sun[2])/l; (0.35+0.65*lam.max(0.0)/sun[1]).min(1.35)};
    let c=color(h); for k in 0..3{ buf.push((c[k]*shade).clamp(0.0,255.0) as u8);} }}
  let mut f=std::fs::File::create(out).unwrap(); write!(f,"P6\n{n} {n}\n255\n").unwrap(); f.write_all(&buf).unwrap();
  let mut land:Vec<f32>=hs.iter().copied().filter(|&h|h>=63.5).map(|h|h-64.0).collect(); land.sort_by(f32::total_cmp);
  let q=|p:f64| if land.is_empty(){0.0}else{land[((land.len()-1) as f64*p) as usize]};
  println!("variant {variant} span {span}: land {:.1}%  land height above sea p50 {:.0} p90 {:.0} p99 {:.0} max {:.0}",100.0*land.len() as f64/(n*n) as f64,q(0.5),q(0.9),q(0.99),q(1.0));
}
