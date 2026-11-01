import test from 'node:test';
import assert from 'node:assert/strict';
import {projection,containsRectangle,servicePositions,tileCoverage} from './geometry.js';
const shape={type:'Polygon',coordinates:[[[-76.52,3.48],[-76.519,3.48],[-76.519,3.481],[-76.52,3.481],[-76.52,3.48]]]};
test('local projection round trip preserves georeferencing',()=>{const p=projection(shape),point=[-76.5198,3.4807],back=p.unproject(p.point(point));assert.ok(Math.abs(back[0]-point[0])<1e-12);assert.ok(Math.abs(back[1]-point[1])<1e-12);});
test('service boxes never cross a concave notch or internal hole',()=>{const poly=[[[0,0],[20,0],[20,20],[12,20],[12,8],[8,8],[8,20],[0,20],[0,0]]];assert.equal(containsRectangle(10,12,8,12,[poly]),false);const hole=[[[0,0],[20,0],[20,20],[0,20],[0,0]],[[9,9],[11,9],[11,11],[9,11],[9,9]]];assert.equal(containsRectangle(10,10,10,10,[hole]),false);assert.equal(containsRectangle(3,3,2,2,[hole]),true);});
test('zero requested services do not create fictitious facilities',()=>{assert.deepEqual(servicePositions(shape,projection(shape),0),[]);});
test('aerial context uses at most nine viewport tiles and covers footprint',()=>{const tiles=tileCoverage(shape,9);assert.ok(tiles.length>0&&tiles.length<=9);for(const [lon,lat]of shape.coordinates[0])assert.ok(tiles.some(t=>lon>=t.west&&lon<=t.east&&lat>=t.south&&lat<=t.north));});
