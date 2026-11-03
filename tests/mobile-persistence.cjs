// Synthetic fixture only. No Spotify login, model call or real export.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert=require('node:assert/strict');
(async()=>{
  const browser=await chromium.launch({headless:true,channel:'chrome'});
  try {
    const context=await browser.newContext({viewport:{width:390,height:850},isMobile:true,hasTouch:true});
    await context.addCookies([{name:'promptlist_session',value:'fixture',url:'http://127.0.0.1:8788',httpOnly:true,sameSite:'Lax'}]);
    const page=await context.newPage();
    const errors=[]; page.on('pageerror',e=>errors.push(e.message));
    const response=await page.goto('http://127.0.0.1:8788/');
    assert.match((await response.allHeaders())['set-cookie'],/Max-Age=7776000/);
    await page.locator('#prompt').fill('8 tracks');
    await page.getByRole('button',{name:'Build preview',exact:true}).click();
    await page.waitForFunction(()=>document.querySelectorAll('.track-row').length===8);
    assert((await page.locator('.save-form input[name=name]').inputValue()).split(/\s+/).length<=4);
    assert.equal(await page.locator('.composer-form input[name=name]').count(),0);
    for(const width of [320,390,1024,1440]) {
      const viewportContext=await browser.newContext({viewport:{width,height:850},isMobile:width<1024,hasTouch:width<1024});
      await viewportContext.addCookies(await context.cookies());
      const viewportPage=await viewportContext.newPage();
      if(width===320) await viewportPage.route('**/api/playback/token',route=>route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:'Synthetic SDK connection failure'})}));
      await viewportPage.goto('http://127.0.0.1:8788/');
      if(width===320) {
        await viewportPage.waitForFunction(()=>document.querySelector('#playback-status').textContent==='Synthetic SDK connection failure');
        assert.equal(await viewportPage.locator('.preview-track').first().isEnabled(),true,'track button must allow connection retry while player is hidden');
        await viewportPage.unroute('**/api/playback/token');
      } else await viewportPage.locator('#radio-player[data-fixture-connections="1"]').waitFor({state:'attached'});
      assert.equal(await viewportPage.locator('#radio-player').isVisible(),false);
      const idleFooter=await viewportPage.locator('body > footer').evaluate(el=>el.getBoundingClientRect().height);
      if(width<1024) {
        assert.equal(idleFooter,0,'idle footer must occupy no space');
        assert.equal(await viewportPage.locator('main').evaluate(el=>el.getBoundingClientRect().bottom),850,'idle workspace must reach viewport bottom');
      }
      await viewportPage.screenshot({path:`/tmp/promptlist-player-idle-${width}.png`});
      if(width===390) {
        await viewportPage.route('**/api/playback/play',route=>route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:'Synthetic playback failure'})}));
        await viewportPage.locator('.preview-track').first().click();
        await viewportPage.waitForFunction(()=>document.querySelector('#playback-status').textContent==='Synthetic playback failure');
        assert.equal(await viewportPage.locator('#radio-player').isVisible(),false,'failed playback must not expose idle player');
        await viewportPage.unroute('**/api/playback/play');
      }
      await viewportPage.locator('.preview-track').first().click();
      await viewportPage.getByRole('button',{name:'Pause',exact:true}).waitFor();
      assert.equal(await viewportPage.locator('#radio-player').isVisible(),true);
      await viewportPage.getByRole('button',{name:'Pause',exact:true}).click();
      assert.equal(await viewportPage.locator('#radio-player').isVisible(),true,'paused player stays available');
      await viewportPage.getByRole('link',{name:'Library',exact:true}).click();
      await viewportPage.locator('main[data-view="library"]').waitFor();
      assert.equal(await viewportPage.locator('#radio-player').isVisible(),true,'player persists through navigation');
      await viewportPage.getByRole('link',{name:'Build a playlist',exact:true}).click();
      await viewportPage.locator('#prompt').waitFor();
      await viewportPage.evaluate(async()=>{await document.fonts.ready;await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));});
      const metrics=await viewportPage.evaluate(()=>({
        playerBottom:document.querySelector('#radio-player').getBoundingClientRect().bottom,
        playerPosition:getComputedStyle(document.querySelector('#radio-player')).position,
        playerHeight:document.querySelector('#radio-player').getBoundingClientRect().height,
        playerBlur:getComputedStyle(document.querySelector('#radio-player')).backdropFilter,
        playerBackground:getComputedStyle(document.querySelector('#radio-player')).backgroundColor,
        exportColor:getComputedStyle(document.querySelector('.export-button')).backgroundColor,
        headerImage:getComputedStyle(document.querySelector('body > header')).backgroundImage,
        backgroundPosition:getComputedStyle(document.body,'::before').position,
        overflow:document.documentElement.scrollWidth>innerWidth,
        shadow:getComputedStyle(document.querySelector('#prompt')).boxShadow
      }));
      assert.equal(metrics.headerImage,'none'); assert.equal(metrics.backgroundPosition,'fixed');
      assert.equal(metrics.overflow,false); assert.equal(metrics.shadow,'none');
      assert.equal(metrics.playerBlur,'blur(18px)');
      assert.equal(metrics.playerBackground,'rgba(42, 44, 43, 0.82)');
      assert.equal(metrics.exportColor,'rgb(170, 169, 112)');
      if(width<1024) {
        assert.equal(metrics.playerPosition,'fixed');assert(metrics.playerBottom<=851 && metrics.playerBottom>=849);
        assert.equal(metrics.playerHeight,60,'mobile player must occupy only 60px');
        const playingFooter=await viewportPage.locator('body > footer').evaluate(el=>el.getBoundingClientRect().height);
        assert.equal(playingFooter,metrics.playerHeight,'footer reserves exactly the player height, no extra strip');
        assert.equal(await viewportPage.locator('main').evaluate(el=>el.getBoundingClientRect().bottom),850-metrics.playerHeight);
      }
      assert.equal(await viewportPage.locator('body > header').count(),1);
      if(width>=1024) {
        const exportBox=await viewportPage.getByRole('button',{name:'Export to Spotify',exact:true}).boundingBox();
        assert(exportBox.y+exportBox.height<=850,'desktop export must fit the viewport');
      }
      await viewportPage.screenshot({path:`/tmp/promptlist-mobile-release-${width}.png`});
      await viewportContext.close();
    }
    await page.setViewportSize({width:390,height:850});
    // Exercise native scroll chaining at both queue boundaries, not just CSS values.
    const queue=page.getByRole('region',{name:'Playlist tracks',exact:true});
    assert.equal(await queue.evaluate(el=>getComputedStyle(el).overscrollBehaviorY),'auto');
    const touch=await context.newCDPSession(page);
    for(const [gesture,direction] of [['wheel',1],['wheel',-1],['touch',1],['touch',-1]]) {
      await queue.evaluate((el,direction)=>{
        const main=document.querySelector('main');
        main.scrollTop+=el.getBoundingClientRect().top-main.getBoundingClientRect().top-60;
        el.scrollTop=direction>0?el.scrollHeight:0;
      },direction);
      const beforeScroll=await page.locator('main').evaluate(el=>el.scrollTop);
      const box=await queue.boundingBox();
      const x=box.x+box.width/2,y=Math.max(160,box.y+100);
      if(gesture==='touch') await touch.send('Input.synthesizeScrollGesture',{x,y,yDistance:-direction*100,gestureSourceType:'touch',speed:400,preventFling:true});
      else {await page.mouse.move(x,y);await page.mouse.wheel(0,direction*180);}
      await page.waitForFunction(({beforeScroll,direction})=>(document.querySelector('main').scrollTop-beforeScroll)*direction>20,{beforeScroll,direction});
    }
    let edits=0;
    await page.route('**/preview/edit',async route=>{
      edits++;
      await new Promise(resolve=>setTimeout(resolve,800));
      await route.continue();
    });
    const before=await page.locator('#selection').getAttribute('data-preview-id');
    for(let i=0;i<3;i++) await page.getByRole('button',{name:'Remove track',exact:true}).first().click();
    assert.equal(await page.locator('.track-row').count(),5,'removals must appear before network completes');
    assert(await page.getByRole('button',{name:'Export to Spotify',exact:true}).isDisabled());
    await page.waitForFunction(()=>document.querySelector('.preview').getAttribute('aria-busy')==='false');
    assert.notEqual(await page.locator('#selection').getAttribute('data-preview-id'),before);
    assert(edits<=2,'rapid clicks should coalesce');
    await page.reload(); assert.equal(await page.locator('.track-row').count(),5);
    await page.unroute('**/preview/edit');
    let failures=0;
    await page.route('**/preview/edit',async route=>{failures++;await route.abort();});
    await page.getByRole('button',{name:'Remove track',exact:true}).first().click();
    await page.waitForFunction(()=>document.querySelectorAll('.track-row').length===5 && document.querySelector('.preview').getAttribute('aria-busy')==='false');
    assert.equal(failures,1,'ambiguous edits must not retry');
    await page.unroute('**/preview/edit');
    await page.locator('#prompt').fill('Keep this unfinished prompt');
    await page.evaluate(()=>{
      document.querySelector('.build-button').disabled=true;
      window.dispatchEvent(new PageTransitionEvent('pageshow',{persisted:true}));
    });
    await page.waitForFunction(()=>!document.querySelector('.build-button').disabled);
    assert.equal(await page.locator('#prompt').inputValue(),'Keep this unfinished prompt');
    assert.deepEqual(errors,[]);
    console.log('PASS: hidden-until-playing frosted compact player, pause/navigation persistence, failed-playback visibility, export accent, bidirectional scroll handoff, 90-day cookie, batched removals, BFCache; 320/390/1024/1440.');
  } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;});
