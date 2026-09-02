(self.webpackChunkmyssa_statement_ui = self.webpackChunkmyssa_statement_ui || []).push([[9524], {
    9524(B, G, p) {
        "use strict";
        var f = p(6697)
          , e = p(1015);
        class k {
            _doc;
            constructor(i) {
                this._doc = i
            }
            manager
        }
        let b = ( () => {
            class r extends k {
                constructor(t) {
                    super(t)
                }
                supports(t) {
                    return !0
                }
                addEventListener(t, n, o, s) {
                    return t.addEventListener(n, o, s),
                    () => this.removeEventListener(t, n, o, s)
                }
                removeEventListener(t, n, o, s) {
                    return t.removeEventListener(n, o, s)
                }
                static \u0275fac = function(n) {
                    return new (n || r)(e.\u0275\u0275inject(f.DOCUMENT))
                }
                ;
                static \u0275prov = e.\u0275\u0275defineInjectable({
                    token: r,
                    factory: r.\u0275fac
                })
            }
            return r
        }
        )();
        const T = new e.InjectionToken("");
        let C = ( () => {
            class r {
                _zone;
                _plugins;
                _eventNameToPlugin = new Map;
                constructor(t, n) {
                    this._zone = n,
                    t.forEach(a => {
                        a.manager = this
                    }
                    );
                    const o = t.filter(a => !(a instanceof b));
                    this._plugins = o.slice().reverse();
                    const s = t.find(a => a instanceof b);
                    s && this._plugins.push(s)
                }
                addEventListener(t, n, o, s) {
                    return this._findPluginFor(n).addEventListener(t, n, o, s)
                }
                getZone() {
                    return this._zone
                }
                _findPluginFor(t) {
                    let n = this._eventNameToPlugin.get(t);
                    if (n)
                        return n;
                    if (n = this._plugins.find(s => s.supports(t)),
                    !n)
                        throw new e.\u0275RuntimeError(5101,!1);
                    return this._eventNameToPlugin.set(t, n),
                    n
                }
                static \u0275fac = function(n) {
                    return new (n || r)(e.\u0275\u0275inject(T),e.\u0275\u0275inject(e.NgZone))
                }
                ;
                static \u0275prov = e.\u0275\u0275defineInjectable({
                    token: r,
                    factory: r.\u0275fac
                })
            }
            return r
        }
        )();
        const g = "ng-app-id";
        function O(r) {
            for (const i of r)
                i.remove()
        }
        function M(r, i) {
            const t = i.createElement("style");
            return t.textContent = r,
            t
        }
        function c(r, i) {
            const t = i.createElement("link");
            return t.setAttribute("rel", "stylesheet"),
            t.setAttribute("href", r),
            t
        }
        let _ = ( () => {
            class r {
                doc;
                appId;
                nonce;
                inline = new Map;
                external = new Map;
                hosts = new Set;
                constructor(t, n, o, s={}) {
                    this.doc = t,
                    this.appId = n,
                    this.nonce = o,
                    function l(r, i, t, n) {
                        const o = r.head?.querySelectorAll(`style[${g}="${i}"],link[${g}="${i}"]`);
                        if (o)
                            for (const s of o)
                                s.removeAttribute(g),
                                s instanceof HTMLLinkElement ? n.set(s.href.slice(s.href.lastIndexOf("/") + 1), {
                                    usage: 0,
                                    elements: [s]
                                }) : s.textContent && t.set(s.textContent, {
                                    usage: 0,
                                    elements: [s]
                                })
                    }(t, n, this.inline, this.external),
                    this.hosts.add(t.head)
                }
                addStyles(t, n) {
                    for (const o of t)
                        this.addUsage(o, this.inline, M);
                    n?.forEach(o => this.addUsage(o, this.external, c))
                }
                removeStyles(t, n) {
                    for (const o of t)
                        this.removeUsage(o, this.inline);
                    n?.forEach(o => this.removeUsage(o, this.external))
                }
                addUsage(t, n, o) {
                    const s = n.get(t);
                    s ? s.usage++ : n.set(t, {
                        usage: 1,
                        elements: [...this.hosts].map(a => this.addElement(a, o(t, this.doc)))
                    })
                }
                removeUsage(t, n) {
                    const o = n.get(t);
                    o && (o.usage--,
                    o.usage <= 0 && (O(o.elements),
                    n.delete(t)))
                }
                ngOnDestroy() {
                    for (const [,{elements: t}] of [...this.inline, ...this.external])
                        O(t);
                    this.hosts.clear()
                }
                addHost(t) {
                    this.hosts.add(t);
                    for (const [n,{elements: o}] of this.inline)
                        o.push(this.addElement(t, M(n, this.doc)));
                    for (const [n,{elements: o}] of this.external)
                        o.push(this.addElement(t, c(n, this.doc)))
                }
                removeHost(t) {
                    this.hosts.delete(t)
                }
                addElement(t, n) {
                    return this.nonce && n.setAttribute("nonce", this.nonce),
                    t.appendChild(n)
                }
                static \u0275fac = function(n) {
                    return new (n || r)(e.\u0275\u0275inject(f.DOCUMENT),e.\u0275\u0275inject(e.APP_ID),e.\u0275\u0275inject(e.CSP_NONCE, 8),e.\u0275\u0275inject(e.PLATFORM_ID))
                }
                ;
                static \u0275prov = e.\u0275\u0275defineInjectable({
                    token: r,
                    factory: r.\u0275fac
                })
            }
            return r
        }
        )();
        const u = {
            svg: "http://www.w3.org/2000/svg",
            xhtml: "http://www.w3.org/1999/xhtml",
            xlink: "http://www.w3.org/1999/xlink",
            xml: "http://www.w3.org/XML/1998/namespace",
            xmlns: "http://www.w3.org/2000/xmlns/",
            math: "http://www.w3.org/1998/Math/MathML"
        }
          , m = /%COMP%/g
          , D = "%COMP%"
          , P = `_nghost-${D}`
          , L = `_ngcontent-${D}`
          , se = new e.InjectionToken("",{
            providedIn: "root",
            factory: () => !0
        });
        function Z(r, i) {
            return i.map(t => t.replace(m, r))
        }
        let z = ( () => {
            class r {
                eventManager;
                sharedStylesHost;
                appId;
                removeStylesOnCompDestroy;
                doc;
                ngZone;
                nonce;
                tracingService;
                rendererByCompId = new Map;
                defaultRenderer;
                platformIsServer;
                constructor(t, n, o, s, a, d, v=null, h=null) {
                    this.eventManager = t,
                    this.sharedStylesHost = n,
                    this.appId = o,
                    this.removeStylesOnCompDestroy = s,
                    this.doc = a,
                    this.ngZone = d,
                    this.nonce = v,
                    this.tracingService = h,
                    this.platformIsServer = !1,
                    this.defaultRenderer = new H(t,a,d,this.platformIsServer,this.tracingService)
                }
                createRenderer(t, n) {
                    if (!t || !n)
                        return this.defaultRenderer;
                    const o = this.getOrCreateRenderer(t, n);
                    return o instanceof K ? o.applyToHost(t) : o instanceof V && o.applyStyles(),
                    o
                }
                getOrCreateRenderer(t, n) {
                    const o = this.rendererByCompId;
                    let s = o.get(n.id);
                    if (!s) {
                        const a = this.doc
                          , d = this.ngZone
                          , v = this.eventManager
                          , h = this.sharedStylesHost
                          , y = this.removeStylesOnCompDestroy
                          , A = this.platformIsServer
                          , E = this.tracingService;
                        switch (n.encapsulation) {
                        case e.ViewEncapsulation.Emulated:
                            s = new K(v,h,n,this.appId,y,a,d,A,E);
                            break;
                        case e.ViewEncapsulation.ShadowDom:
                            return new ue(v,h,t,n,a,d,this.nonce,A,E);
                        default:
                            s = new V(v,h,n,y,a,d,A,E)
                        }
                        o.set(n.id, s)
                    }
                    return s
                }
                ngOnDestroy() {
                    this.rendererByCompId.clear()
                }
                componentReplaced(t) {
                    this.rendererByCompId.delete(t)
                }
                static \u0275fac = function(n) {
                    return new (n || r)(e.\u0275\u0275inject(C),e.\u0275\u0275inject(_),e.\u0275\u0275inject(e.APP_ID),e.\u0275\u0275inject(se),e.\u0275\u0275inject(f.DOCUMENT),e.\u0275\u0275inject(e.NgZone),e.\u0275\u0275inject(e.CSP_NONCE),e.\u0275\u0275inject(e.\u0275TracingService, 8))
                }
                ;
                static \u0275prov = e.\u0275\u0275defineInjectable({
                    token: r,
                    factory: r.\u0275fac
                })
            }
            return r
        }
        )();
        class H {
            eventManager;
            doc;
            ngZone;
            platformIsServer;
            tracingService;
            data = Object.create(null);
            throwOnSyntheticProps = !0;
            constructor(i, t, n, o, s) {
                this.eventManager = i,
                this.doc = t,
                this.ngZone = n,
                this.platformIsServer = o,
                this.tracingService = s
            }
            destroy() {}
            destroyNode = null;
            createElement(i, t) {
                return t ? this.doc.createElementNS(u[t] || t, i) : this.doc.createElement(i)
            }
            createComment(i) {
                return this.doc.createComment(i)
            }
            createText(i) {
                return this.doc.createTextNode(i)
            }
            appendChild(i, t) {
                (Y(i) ? i.content : i).appendChild(t)
            }
            insertBefore(i, t, n) {
                i && (Y(i) ? i.content : i).insertBefore(t, n)
            }
            removeChild(i, t) {
                t.remove()
            }
            selectRootElement(i, t) {
                let n = "string" == typeof i ? this.doc.querySelector(i) : i;
                if (!n)
                    throw new e.\u0275RuntimeError(-5104,!1);
                return t || (n.textContent = ""),
                n
            }
            parentNode(i) {
                return i.parentNode
            }
            nextSibling(i) {
                return i.nextSibling
            }
            setAttribute(i, t, n, o) {
                if (o) {
                    t = o + ":" + t;
                    const s = u[o];
                    s ? i.setAttributeNS(s, t, n) : i.setAttribute(t, n)
                } else
                    i.setAttribute(t, n)
            }
            removeAttribute(i, t, n) {
                if (n) {
                    const o = u[n];
                    o ? i.removeAttributeNS(o, t) : i.removeAttribute(`${n}:${t}`)
                } else
                    i.removeAttribute(t)
            }
            addClass(i, t) {
                i.classList.add(t)
            }
            removeClass(i, t) {
                i.classList.remove(t)
            }
            setStyle(i, t, n, o) {
                o & (e.RendererStyleFlags2.DashCase | e.RendererStyleFlags2.Important) ? i.style.setProperty(t, n, o & e.RendererStyleFlags2.Important ? "important" : "") : i.style[t] = n
            }
            removeStyle(i, t, n) {
                n & e.RendererStyleFlags2.DashCase ? i.style.removeProperty(t) : i.style[t] = ""
            }
            setProperty(i, t, n) {
                null != i && (i[t] = n)
            }
            setValue(i, t) {
                i.nodeValue = t
            }
            listen(i, t, n, o) {
                if ("string" == typeof i && !(i = (0,
                f.\u0275getDOM)().getGlobalEventTarget(this.doc, i)))
                    throw new e.\u0275RuntimeError(5102,!1);
                let s = this.decoratePreventDefault(n);
                return this.tracingService?.wrapEventListener && (s = this.tracingService.wrapEventListener(i, t, s)),
                this.eventManager.addEventListener(i, t, s, o)
            }
            decoratePreventDefault(i) {
                return t => {
                    if ("__ngUnwrap__" === t)
                        return i;
                    !1 === i(t) && t.preventDefault()
                }
            }
        }
        function Y(r) {
            return "TEMPLATE" === r.tagName && void 0 !== r.content
        }
        class ue extends H {
            sharedStylesHost;
            hostEl;
            shadowRoot;
            constructor(i, t, n, o, s, a, d, v, h) {
                super(i, s, a, v, h),
                this.sharedStylesHost = t,
                this.hostEl = n,
                this.shadowRoot = n.attachShadow({
                    mode: "open"
                }),
                this.sharedStylesHost.addHost(this.shadowRoot);
                let y = o.styles;
                y = Z(o.id, y);
                for (const E of y) {
                    const R = document.createElement("style");
                    d && R.setAttribute("nonce", d),
                    R.textContent = E,
                    this.shadowRoot.appendChild(R)
                }
                const A = o.getExternalStyles?.();
                if (A)
                    for (const E of A) {
                        const R = c(E, s);
                        d && R.setAttribute("nonce", d),
                        this.shadowRoot.appendChild(R)
                    }
            }
            nodeOrShadowRoot(i) {
                return i === this.hostEl ? this.shadowRoot : i
            }
            appendChild(i, t) {
                return super.appendChild(this.nodeOrShadowRoot(i), t)
            }
            insertBefore(i, t, n) {
                return super.insertBefore(this.nodeOrShadowRoot(i), t, n)
            }
            removeChild(i, t) {
                return super.removeChild(null, t)
            }
            parentNode(i) {
                return this.nodeOrShadowRoot(super.parentNode(this.nodeOrShadowRoot(i)))
            }
            destroy() {
                this.sharedStylesHost.removeHost(this.shadowRoot)
            }
        }
        class V extends H {
            sharedStylesHost;
            removeStylesOnCompDestroy;
            styles;
            styleUrls;
            constructor(i, t, n, o, s, a, d, v, h) {
                super(i, s, a, d, v),
                this.sharedStylesHost = t,
                this.removeStylesOnCompDestroy = o;
                let y = n.styles;
                this.styles = h ? Z(h, y) : y,
                this.styleUrls = n.getExternalStyles?.(h)
            }
            applyStyles() {
                this.sharedStylesHost.addStyles(this.styles, this.styleUrls)
            }
            destroy() {
                this.removeStylesOnCompDestroy && 0 === e.\u0275allLeavingAnimations.size && this.sharedStylesHost.removeStyles(this.styles, this.styleUrls)
            }
        }
        class K extends V {
            contentAttr;
            hostAttr;
            constructor(i, t, n, o, s, a, d, v, h) {
                const y = o + "-" + n.id;
                super(i, t, n, s, a, d, v, h, y),
                this.contentAttr = function ae(r) {
                    return L.replace(m, r)
                }(y),
                this.hostAttr = function le(r) {
                    return P.replace(m, r)
                }(y)
            }
            applyToHost(i) {
                this.applyStyles(),
                this.setAttribute(i, this.hostAttr, "")
            }
            createElement(i, t) {
                const n = super.createElement(i, t);
                return super.setAttribute(n, this.contentAttr, ""),
                n
            }
        }
        class X extends f.\u0275DomAdapter {
            supportsDOMEvents = !0;
            static makeCurrent() {
                (0,
                f.\u0275setRootDomAdapter)(new X)
            }
            onAndCancel(i, t, n, o) {
                return i.addEventListener(t, n, o),
                () => {
                    i.removeEventListener(t, n, o)
                }
            }
            dispatchEvent(i, t) {
                i.dispatchEvent(t)
            }
            remove(i) {
                i.remove()
            }
            createElement(i, t) {
                return (t = t || this.getDefaultDocument()).createElement(i)
            }
            createHtmlDocument() {
                return document.implementation.createHTMLDocument("fakeTitle")
            }
            getDefaultDocument() {
                return document
            }
            isElementNode(i) {
                return i.nodeType === Node.ELEMENT_NODE
            }
            isShadowRoot(i) {
                return i instanceof DocumentFragment
            }
            getGlobalEventTarget(i, t) {
                return "window" === t ? window : "document" === t ? i : "body" === t ? i.body : null
            }
            getBaseHref(i) {
                const t = function de() {
                    return F = F || document.head.querySelector("base"),
                    F ? F.getAttribute("href") : null
                }();
                return null == t ? null : function fe(r) {
                    return new URL(r,document.baseURI).pathname
                }(t)
            }
            resetBaseElement() {
                F = null
            }
            getUserAgent() {
                return window.navigator.userAgent
            }
            getCookie(i) {
                return (0,
                f.\u0275parseCookieValue)(document.cookie, i)
            }
        }
        let F = null
          , me = ( () => {
            class r {
                build() {
                    return new XMLHttpRequest
                }
                static \u0275fac = function(n) {
                    return new (n || r)
                }
                ;
                static \u0275prov = e.\u0275\u0275defineInjectable({
                    token: r,
                    factory: r.\u0275fac
                })
            }
            return r
        }
        )();
        const J = ["alt", "control", "meta", "shift"]
          , he = {
            "\b": "Backspace",
            "\t": "Tab",
            "\x7f": "Delete",
            "\x1b": "Escape",
            Del: "Delete",
            Esc: "Escape",
            Left: "ArrowLeft",
            Right: "ArrowRight",
            Up: "ArrowUp",
            Down: "ArrowDown",
            Menu: "ContextMenu",
            Scroll: "ScrollLock",
            Win: "OS"
        }
          , ye = {
            alt: r => r.altKey,
            control: r => r.ctrlKey,
            meta: r => r.metaKey,
            shift: r => r.shiftKey
        };
        let ve = ( () => {
            class r extends k {
                constructor(t) {
                    super(t)
                }
                supports(t) {
                    return null != r.parseEventName(t)
                }
                addEventListener(t, n, o, s) {
                    const a = r.parseEventName(n)
                      , d = r.eventCallback(a.fullKey, o, this.manager.getZone());
                    return this.manager.getZone().runOutsideAngular( () => (0,
                    f.\u0275getDOM)().onAndCancel(t, a.domEventName, d, s))
                }
                static parseEventName(t) {
                    const n = t.toLowerCase().split(".")
                      , o = n.shift();
                    if (0 === n.length || "keydown" !== o && "keyup" !== o)
                        return null;
                    const s = r._normalizeKey(n.pop());
                    let a = ""
                      , d = n.indexOf("code");
                    if (d > -1 && (n.splice(d, 1),
                    a = "code."),
                    J.forEach(h => {
                        const y = n.indexOf(h);
                        y > -1 && (n.splice(y, 1),
                        a += h + ".")
                    }
                    ),
                    a += s,
                    0 != n.length || 0 === s.length)
                        return null;
                    const v = {};
                    return v.domEventName = o,
                    v.fullKey = a,
                    v
                }
                static matchEventFullKeyCode(t, n) {
                    let o = he[t.key] || t.key
                      , s = "";
                    return n.indexOf("code.") > -1 && (o = t.code,
                    s = "code."),
                    !(null == o || !o) && (o = o.toLowerCase(),
                    " " === o ? o = "space" : "." === o && (o = "dot"),
                    J.forEach(a => {
                        a !== o && (0,
                        ye[a])(t) && (s += a + ".")
                    }
                    ),
                    s += o,
                    s === n)
                }
                static eventCallback(t, n, o) {
                    return s => {
                        r.matchEventFullKeyCode(s, t) && o.runGuarded( () => n(s))
                    }
                }
                static _normalizeKey(t) {
                    return "esc" === t ? "escape" : t
                }
                static \u0275fac = function(n) {
                    return new (n || r)(e.\u0275\u0275inject(f.DOCUMENT))
                }
                ;
                static \u0275prov = e.\u0275\u0275defineInjectable({
                    token: r,
                    factory: r.\u0275fac
                })
            }
            return r
        }
        )();
        const be = (0,
        e.createPlatformFactory)(e.platformCore, "browser", [{
            provide: e.PLATFORM_ID,
            useValue: f.\u0275PLATFORM_BROWSER_ID
        }, {
            provide: e.PLATFORM_INITIALIZER,
            useValue: function ge() {
                X.makeCurrent()
            },
            multi: !0
        }, {
            provide: f.DOCUMENT,
            useFactory: function Se() {
                return (0,
                e.\u0275setDocument)(document),
                document
            }
        }])
          , te = [{
            provide: e.\u0275TESTABILITY_GETTER,
            useClass: class pe {
                addToWindow(i) {
                    e.\u0275global.getAngularTestability = (n, o=!0) => {
                        const s = i.findTestabilityInTree(n, o);
                        if (null == s)
                            throw new e.\u0275RuntimeError(5103,!1);
                        return s
                    }
                    ,
                    e.\u0275global.getAllAngularTestabilities = () => i.getAllTestabilities(),
                    e.\u0275global.getAllAngularRootElements = () => i.getAllRootElements(),
                    e.\u0275global.frameworkStabilizers || (e.\u0275global.frameworkStabilizers = []),
                    e.\u0275global.frameworkStabilizers.push(n => {
                        const o = e.\u0275global.getAllAngularTestabilities();
                        let s = o.length;
                        const a = function() {
                            s--,
                            0 == s && n()
                        };
                        o.forEach(d => {
                            d.whenStable(a)
                        }
                        )
                    }
                    )
                }
                findTestabilityInTree(i, t, n) {
                    return null == t ? null : i.getTestability(t) ?? (n ? (0,
                    f.\u0275getDOM)().isShadowRoot(t) ? this.findTestabilityInTree(i, t.host, !0) : this.findTestabilityInTree(i, t.parentElement, !0) : null)
                }
            }
        }, {
            provide: e.\u0275TESTABILITY,
            useClass: e.Testability,
            deps: [e.NgZone, e.TestabilityRegistry, e.\u0275TESTABILITY_GETTER]
        }, {
            provide: e.Testability,
            useClass: e.Testability,
            deps: [e.NgZone, e.TestabilityRegistry, e.\u0275TESTABILITY_GETTER]
        }]
          , ne = [{
            provide: e.\u0275INJECTOR_SCOPE,
            useValue: "root"
        }, {
            provide: e.ErrorHandler,
            useFactory: function _e() {
                return new e.ErrorHandler
            }
        }, {
            provide: T,
            useClass: b,
            multi: !0,
            deps: [f.DOCUMENT]
        }, {
            provide: T,
            useClass: ve,
            multi: !0,
            deps: [f.DOCUMENT]
        }, z, _, C, {
            provide: e.RendererFactory2,
            useExisting: z
        }, {
            provide: f.XhrFactory,
            useClass: me
        }, []];
        let we = ( () => {
            class r {
                constructor() {}
                static \u0275fac = function(n) {
                    return new (n || r)
                }
                ;
                static \u0275mod = e.\u0275\u0275defineNgModule({
                    type: r
                });
                static \u0275inj = e.\u0275\u0275defineInjector({
                    providers: [...ne, ...te],
                    imports: [f.CommonModule, e.ApplicationModule]
                })
            }
            return r
        }
        )();
        var Ee = p(7209)
          , U = p(4997)
          , Ce = p(467)
          , j = p(8485);
        const w_contactUsUrl = "https://www.ssa.gov/agency/contact/phone.html";
        var re = p(9437)
          , Me = p(9350);
        function W(r, i) {
            const t = "object" == typeof i;
            return new Promise( (n, o) => {
                let a, s = !1;
                r.subscribe({
                    next: d => {
                        a = d,
                        s = !0
                    }
                    ,
                    error: o,
                    complete: () => {
                        s ? n(a) : t ? n(i.defaultValue) : o(new Me.G)
                    }
                })
            }
            )
        }
        var oe = p(8810)
          , S = p(2777)
          , xe = p(8141);
        let ie = ( () => {
            class r {
                _bffApiBaseUrl;
                _http;
                constructor(t, n) {
                    this._bffApiBaseUrl = t,
                    this._http = n
                }
                getHeaderV2() {
                    return this._http.get(this._bffApiBaseUrl + "/headerInfoV2", {
                        withCredentials: !0
                    }).pipe((0,
                    xe.M)({
                        next: t => console.log("Service response:", t),
                        error: t => console.error("Service error:", t)
                    }))
                }
                checkStatementAvailability() {
                    return this._http.get(this._bffApiBaseUrl + "/statementAvailability", {
                        withCredentials: !0
                    })
                }
                checkStatementXmlAvailability() {
                    return this._http.get(this._bffApiBaseUrl + "/statementXmlAvailability", {
                        withCredentials: !0
                    })
                }
                writeMiWhenLoadingHomePage(t) {
                    return this._http.post(this._bffApiBaseUrl + "/writeMiWhenLoadingHomePage", t, {
                        withCredentials: !0
                    })
                }
                static \u0275fac = function(n) {
                    return new (n || r)(e.\u0275\u0275inject(S.BFF_API_BASE_URL),e.\u0275\u0275inject(j.HttpClient))
                }
                ;
                static \u0275prov = e.\u0275\u0275defineInjectable({
                    token: r,
                    factory: r.\u0275fac,
                    providedIn: "root"
                })
            }
            return r
        }
        )();
        var Ae = p(345)
          , Te = p(2578);
        let $;
        p(6691);
        try {
            $ = !!new Blob
        } catch {
            $ = !1
        }
        let De = ( () => {
            class r {
                get isFileSaverSupported() {
                    return $
                }
                genType(t) {
                    if (!t || -1 === t.lastIndexOf("."))
                        return "text/plain";
                    const n = t.substring(t.lastIndexOf(".") + 1);
                    switch (n) {
                    case "txt":
                        return "text/plain";
                    case "xml":
                    case "html":
                        return `text/${n}`;
                    case "json":
                        return "octet/stream";
                    default:
                        return `application/${n}`
                    }
                }
                save(t, n, o, s) {
                    if (!t)
                        throw new Error("Data argument should be a blob instance");
                    const a = new Blob([t],{
                        type: o || t.type || this.genType(n)
                    });
                    (0,
                    Te.saveAs)(a, decodeURI(n || "download"), s)
                }
                saveText(t, n, o) {
                    const s = new Blob([t]);
                    this.save(s, n, void 0, o)
                }
                static \u0275fac = function(n) {
                    return new (n || r)
                }
                ;
                static \u0275prov = e.\u0275\u0275defineInjectable({
                    token: r,
                    factory: r.\u0275fac,
                    providedIn: "root"
                })
            }
            return r
        }
        )()
          , Re = ( () => {
            class r {
                openDialog;
                static \u0275fac = function(n) {
                    return new (n || r)
                }
                ;
                static \u0275cmp = e.\u0275\u0275defineComponent({
                    type: r,
                    selectors: [["app-how-to-use-xml-files-modal"]],
                    standalone: !1,
                    decls: 34,
                    vars: 0,
                    consts: [["open", "", "backdrop", "", "close-button", "", "width", "690px", "accessibility-text", "", "id", "how-to-use-xml-dialog"], ["slot", "uef-dialog-body"], ["line-height", "var(--uef-font-line-height-regular)"], ["size", "24px", "font-family", "Segoe UI", "color", "#1a1a1a", "alignment", "left"], [2, "margin-top", "0"], [1, "leftAlignBulletPointsAdjustment"], ["target", "_blank", "href", "https://www.ssa.gov/developer/statement/"], ["slot", "uef-dialog-footer", "alignment", "right", "direction", "rtl"], ["fill", "solid"]],
                    template: function(n, o) {
                        1 & n && (e.\u0275\u0275elementStart(0, "uef-dialog", 0)(1, "div", 1)(2, "uef-typography", 2)(3, "uef-typography", 3),
                        e.\u0275\u0275text(4, " About XML Files "),
                        e.\u0275\u0275elementEnd(),
                        e.\u0275\u0275element(5, "br"),
                        e.\u0275\u0275elementStart(6, "p", 4),
                        e.\u0275\u0275text(7, " XML format is a way of saving information so that computer programs can read and use it easily. When you save information in the XML format, you may be able to let other computer programs import it as useful data. These programs might be for financial or retirement planning. "),
                        e.\u0275\u0275elementEnd(),
                        e.\u0275\u0275element(8, "br"),
                        e.\u0275\u0275text(9, " For example, these programs might help you with: "),
                        e.\u0275\u0275elementStart(10, "ul", 5)(11, "li"),
                        e.\u0275\u0275text(12, "Estimating your future retirement benefits"),
                        e.\u0275\u0275elementEnd(),
                        e.\u0275\u0275elementStart(13, "li"),
                        e.\u0275\u0275text(14, "Calculating possible disability and survivors benefits"),
                        e.\u0275\u0275elementEnd(),
                        e.\u0275\u0275elementStart(15, "li"),
                        e.\u0275\u0275text(16, "Recording your earnings history"),
                        e.\u0275\u0275elementEnd(),
                        e.\u0275\u0275elementStart(17, "li"),
                        e.\u0275\u0275text(18, " Estimating the payroll contributions to Social Security and Medicare that you have paid. "),
                        e.\u0275\u0275elementEnd()(),
                        e.\u0275\u0275elementStart(19, "p"),
                        e.\u0275\u0275text(20, " How the Statement data is used will depend on the software program importing the file. "),
                        e.\u0275\u0275elementEnd(),
                        e.\u0275\u0275elementStart(21, "p"),
                        e.\u0275\u0275text(22, " Since this file contains personal information and private financial data, we strongly recommend that you save it in a secure place and share it only with people or institutions that you trust. "),
                        e.\u0275\u0275elementEnd(),
                        e.\u0275\u0275element(23, "br"),
                        e.\u0275\u0275elementStart(24, "p"),
                        e.\u0275\u0275text(25, "To view or print your Statement, use the PDF file link instead."),
                        e.\u0275\u0275elementEnd(),
                        e.\u0275\u0275element(26, "br"),
                        e.\u0275\u0275elementStart(27, "p"),
                        e.\u0275\u0275text(28, " Software developers can find more technical information about the XML file in our "),
                        e.\u0275\u0275elementStart(29, "uef-link", 6),
                        e.\u0275\u0275text(30, " Developers Guide. "),
                        e.\u0275\u0275elementEnd()()()(),
                        e.\u0275\u0275elementStart(31, "uef-button-group", 7)(32, "uef-button", 8),
                        e.\u0275\u0275text(33, "Close"),
                        e.\u0275\u0275elementEnd()()())
                    },
                    styles: [".leftAlignBulletPointsAdjustment[_ngcontent-%COMP%]{padding-left:20px;margin-top:0}"]
                })
            }
            return r
        }
        )();
        function ke(r, i) {
            if (1 & r) {
                const t = e.\u0275\u0275getCurrentView();
                e.\u0275\u0275elementStart(0, "app-how-to-use-xml-files-modal", 18),
                e.\u0275\u0275listener("buttonClick", function() {
                    e.\u0275\u0275restoreView(t);
                    const o = e.\u0275\u0275nextContext(3);
                    return e.\u0275\u0275resetView(o.closeUseXmlDialog())
                })("dialogUpdate", function(o) {
                    e.\u0275\u0275restoreView(t);
                    const s = e.\u0275\u0275nextContext(3);
                    return e.\u0275\u0275resetView(s.openDialog = o.detail.Open)
                }),
                e.\u0275\u0275elementEnd()
            }
        }
        function Oe(r, i) {
            if (1 & r) {
                const t = e.\u0275\u0275getCurrentView();
                e.\u0275\u0275elementStart(0, "div", 13)(1, "uef-typography", 14),
                e.\u0275\u0275text(2, "Additional Links"),
                e.\u0275\u0275elementEnd(),
                e.\u0275\u0275elementStart(3, "p"),
                e.\u0275\u0275element(4, "myssa-file-download", 15)(5, "br")(6, "br"),
                e.\u0275\u0275elementStart(7, "uef-link", 16),
                e.\u0275\u0275listener("linkClick", function() {
                    e.\u0275\u0275restoreView(t);
                    const o = e.\u0275\u0275nextContext(2);
                    return e.\u0275\u0275resetView(o.openUseXmlDialog())
                }),
                e.\u0275\u0275text(8, " How to use XML files "),
                e.\u0275\u0275elementEnd(),
                e.\u0275\u0275template(9, ke, 1, 0, "app-how-to-use-xml-files-modal", 17),
                e.\u0275\u0275elementEnd()()
            }
            if (2 & r) {
                const t = e.\u0275\u0275nextContext(2);
                e.\u0275\u0275advance(4),
                e.\u0275\u0275property("fileFetchFunc", t.statementXmlFileFetchFunc)("fileSaveFunc", t.fileSaveFunc),
                e.\u0275\u0275advance(5),
                e.\u0275\u0275property("ngIf", t.openDialog)
            }
        }
        function Le(r, i) {
            if (1 & r) {
                const t = e.\u0275\u0275getCurrentView();
                e.\u0275\u0275elementStart(0, "uef-container")(1, "uef-container-row")(2, "p")(3, "uef-alert", 8),
                e.\u0275\u0275text(4, " If you are using a public computer, please be aware that when viewing, saving, or printing any documents, the computer you are using may store a temporary copy. "),
                e.\u0275\u0275elementEnd()(),
                e.\u0275\u0275elementStart(5, "uef-typography", 9),
                e.\u0275\u0275element(6, "br"),
                e.\u0275\u0275elementStart(7, "p"),
                e.\u0275\u0275text(8, " Use the link below to access your Statement"),
                e.\u0275\u0275elementEnd()(),
                e.\u0275\u0275elementStart(9, "p"),
                e.\u0275\u0275element(10, "myssa-file-download", 10),
                e.\u0275\u0275elementEnd(),
                e.\u0275\u0275element(11, "br"),
                e.\u0275\u0275elementStart(12, "div"),
                e.\u0275\u0275element(13, "br"),
                e.\u0275\u0275template(14, Oe, 10, 3, "div", 11),
                e.\u0275\u0275elementEnd()(),
                e.\u0275\u0275element(15, "br")(16, "br"),
                e.\u0275\u0275elementStart(17, "uef-container-row")(18, "uef-button-group")(19, "uef-button", 12),
                e.\u0275\u0275listener("click", function() {
                    e.\u0275\u0275restoreView(t);
                    const o = e.\u0275\u0275nextContext();
                    return e.\u0275\u0275resetView(o.exitToMyprofile())
                }),
                e.\u0275\u0275text(20, " Exit "),
                e.\u0275\u0275elementEnd()()()()
            }
            if (2 & r) {
                const t = e.\u0275\u0275nextContext();
                e.\u0275\u0275advance(10),
                e.\u0275\u0275property("fileFetchFunc", t.statementPdfFileFetchFunc)("fileSaveFunc", t.fileSaveFunc),
                e.\u0275\u0275advance(4),
                e.\u0275\u0275property("ngIf", t.isStatementXmlAvailable)
            }
        }
        function Fe(r, i) {
            if (1 & r) {
                const t = e.\u0275\u0275getCurrentView();
                e.\u0275\u0275elementStart(0, "uef-container")(1, "uef-container-row")(2, "uef-alert", 19)(3, "uef-typography", 20),
                e.\u0275\u0275text(4, " Please try again later."),
                e.\u0275\u0275elementEnd()(),
                e.\u0275\u0275element(5, "br")(6, "br"),
                e.\u0275\u0275elementStart(7, "uef-button-group")(8, "uef-button", 12),
                e.\u0275\u0275listener("click", function() {
                    e.\u0275\u0275restoreView(t);
                    const o = e.\u0275\u0275nextContext();
                    return e.\u0275\u0275resetView(o.exitToMyprofile())
                }),
                e.\u0275\u0275text(9, " Back "),
                e.\u0275\u0275elementEnd()()()()
            }
        }
        function Ie(r, i) {
            if (1 & r) {
                const t = e.\u0275\u0275getCurrentView();
                e.\u0275\u0275elementStart(0, "uef-container")(1, "uef-container-row"),
                e.\u0275\u0275element(2, "br"),
                e.\u0275\u0275elementStart(3, "uef-alert", 21)(4, "uef-typography", 20),
                e.\u0275\u0275text(5, " If this problem continues, please "),
                e.\u0275\u0275element(6, "myssa-contact-us-link"),
                e.\u0275\u0275text(7, "."),
                e.\u0275\u0275elementEnd()(),
                e.\u0275\u0275element(8, "br")(9, "br"),
                e.\u0275\u0275elementStart(10, "uef-button-group")(11, "uef-button", 12),
                e.\u0275\u0275listener("click", function() {
                    e.\u0275\u0275restoreView(t);
                    const o = e.\u0275\u0275nextContext();
                    return e.\u0275\u0275resetView(o.exitToMyprofile())
                }),
                e.\u0275\u0275text(12, " Back "),
                e.\u0275\u0275elementEnd()()()()
            }
        }
        function Pe(r, i) {
            if (1 & r) {
                const t = e.\u0275\u0275getCurrentView();
                e.\u0275\u0275elementStart(0, "uef-container", 4)(1, "uef-container-row"),
                e.\u0275\u0275element(2, "br"),
                e.\u0275\u0275elementStart(3, "uef-alert", 21)(4, "uef-typography", 20),
                e.\u0275\u0275text(5, " We cannot provide your Social Security Statement online because you or someone who is eligible to apply on your record recently applied for Social Security benefits or Medicare. You can contact your "),
                e.\u0275\u0275elementStart(6, "uef-link", 22),
                e.\u0275\u0275text(7, "local Social Security office"),
                e.\u0275\u0275elementEnd(),
                e.\u0275\u0275text(8, " to receive your benefit estimates. "),
                e.\u0275\u0275elementEnd()(),
                e.\u0275\u0275element(9, "br")(10, "br"),
                e.\u0275\u0275elementStart(11, "uef-button-group")(12, "uef-button", 12),
                e.\u0275\u0275listener("click", function() {
                    e.\u0275\u0275restoreView(t);
                    const o = e.\u0275\u0275nextContext();
                    return e.\u0275\u0275resetView(o.exitToMyprofile())
                }),
                e.\u0275\u0275text(13, " Back "),
                e.\u0275\u0275elementEnd()()()()
            }
            if (2 & r) {
                const t = e.\u0275\u0275nextContext();
                e.\u0275\u0275advance(6),
                e.\u0275\u0275property("href", t.ofcLocUrl)
            }
        }
        function Ue(r, i) {
            if (1 & r) {
                const t = e.\u0275\u0275getCurrentView();
                e.\u0275\u0275elementStart(0, "uef-container", 5)(1, "uef-container-row"),
                e.\u0275\u0275element(2, "br"),
                e.\u0275\u0275elementStart(3, "uef-alert", 21)(4, "uef-typography", 20),
                e.\u0275\u0275text(5, " Please "),
                e.\u0275\u0275elementStart(6, "uef-link", 22),
                e.\u0275\u0275text(7, "contact us"),
                e.\u0275\u0275elementEnd(),
                e.\u0275\u0275text(8, " to verify any necessary information."),
                e.\u0275\u0275elementEnd()(),
                e.\u0275\u0275element(9, "br")(10, "br"),
                e.\u0275\u0275elementStart(11, "uef-button-group")(12, "uef-button", 12),
                e.\u0275\u0275listener("click", function() {
                    e.\u0275\u0275restoreView(t);
                    const o = e.\u0275\u0275nextContext();
                    return e.\u0275\u0275resetView(o.exitToMyprofile())
                }),
                e.\u0275\u0275text(13, " Back "),
                e.\u0275\u0275elementEnd()()()()
            }
            if (2 & r) {
                const t = e.\u0275\u0275nextContext();
                e.\u0275\u0275advance(6),
                e.\u0275\u0275property("href", t.contactUs)
            }
        }
        function je(r, i) {
            if (1 & r) {
                const t = e.\u0275\u0275getCurrentView();
                e.\u0275\u0275elementStart(0, "uef-container", 6)(1, "uef-container-row"),
                e.\u0275\u0275element(2, "br"),
                e.\u0275\u0275elementStart(3, "uef-alert", 23)(4, "uef-typography", 20),
                e.\u0275\u0275text(5, " If you started working last year, we may not have updated your record to show those earnings yet."),
                e.\u0275\u0275elementEnd()(),
                e.\u0275\u0275element(6, "br")(7, "br"),
                e.\u0275\u0275elementStart(8, "uef-button-group")(9, "uef-button", 12),
                e.\u0275\u0275listener("click", function() {
                    e.\u0275\u0275restoreView(t);
                    const o = e.\u0275\u0275nextContext();
                    return e.\u0275\u0275resetView(o.exitToMyprofile())
                }),
                e.\u0275\u0275text(10, " Back "),
                e.\u0275\u0275elementEnd()()()()
            }
        }
        function Be(r, i) {
            if (1 & r) {
                const t = e.\u0275\u0275getCurrentView();
                e.\u0275\u0275elementStart(0, "uef-container", 7)(1, "uef-container-row"),
                e.\u0275\u0275element(2, "br"),
                e.\u0275\u0275elementStart(3, "uef-alert", 24)(4, "ul", 25)(5, "uef-typography", 20)(6, "li"),
                e.\u0275\u0275text(7, " If you have a pending earnings correction, your Statement should be available after the correction is complete. "),
                e.\u0275\u0275elementEnd(),
                e.\u0275\u0275elementStart(8, "li"),
                e.\u0275\u0275text(9, " If you don\u2019t have a pending earnings correction, we need to verify your earnings record with you. Please "),
                e.\u0275\u0275element(10, "myssa-contact-us-link"),
                e.\u0275\u0275text(11, ". "),
                e.\u0275\u0275elementEnd()()()(),
                e.\u0275\u0275element(12, "br")(13, "br")(14, "br"),
                e.\u0275\u0275elementStart(15, "uef-button-group")(16, "uef-button", 12),
                e.\u0275\u0275listener("click", function() {
                    e.\u0275\u0275restoreView(t);
                    const o = e.\u0275\u0275nextContext();
                    return e.\u0275\u0275resetView(o.exitToMyprofile())
                }),
                e.\u0275\u0275text(17, " Back "),
                e.\u0275\u0275elementEnd()()()()
            }
        }
        const Ne = [{
            path: "",
            redirectTo: "home",
            pathMatch: "full"
        }, {
            path: "home",
            component: ( () => {
                class r {
                    _titleService;
                    _fileSaverService;
                    _httpClient;
                    _bffApiService;
                    _route;
                    _checkStatementXmlAvailabilitySubscription;
                    _writeMiWhenLoadingHomePageSubscription;
                    legacyPebesReturnCode;
                    isStatementAvailable;
                    isStatementXmlAvailable;
                    openDialog = !1;
                    isDownloadingFailed = !1;
                    ofcLocUrl = "https://secure.ssa.gov/ICON/main.jsp";
                    contactUs = w_contactUsUrl;
                    statementPdfFileFetchFunc = () => W(this._httpClient.get("/myssa/myssa-statement-api/statementPdf", {
                        responseType: "blob"
                    }).pipe((0,
                    re.W)(t => (this.isDownloadingFailed = !0,
                    (0,
                    oe.$)( () => t)))));
                    statementXmlFileFetchFunc = () => W(this._httpClient.get("/myssa/myssa-statement-api/statementXml", {
                        responseType: "blob"
                    }).pipe((0,
                    re.W)(t => (this.isDownloadingFailed = !0,
                    (0,
                    oe.$)( () => t)))));
                    exitToStatementPortal() {
                        window.location.href = "/myssa/bec-manage-ui/home"
                    }
                    fileSaveFunc = (t, n, o) => this._fileSaverService.save(t, n, o);
                    constructor(t, n, o, s, a) {
                        this._titleService = t,
                        this._fileSaverService = n,
                        this._httpClient = o,
                        this._bffApiService = s,
                        this._route = a
                    }
                    ngOnInit() {
                        var t = this;
                        return (0,
                        Ce.A)(function*() {
                            setTimeout( () => {
                                t._titleService.setTitle("Social Security Statement - Social Security")
                            }
                            , 500),
                            yield W(t._bffApiService.checkStatementAvailability()).then(s => {
                                t.isStatementAvailable = s.isStatementAvailable,
                                t.legacyPebesReturnCode = s.legacyPebesReturnCode
                            }
                            ).catch(s => t.isStatementAvailable = !1),
                            t.isStatementAvailable && (t._checkStatementXmlAvailabilitySubscription = t._bffApiService.checkStatementXmlAvailability().subscribe({
                                next: s => t.isStatementXmlAvailable = s.statementXmlAvailable,
                                error: s => t.isStatementXmlAvailable = !1
                            })),
                            t._writeMiWhenLoadingHomePageSubscription = t._bffApiService.writeMiWhenLoadingHomePage({
                                referrer: t._route.snapshot.queryParams.ref
                            }).subscribe({
                                next: s => {}
                                ,
                                error: s => {}
                            })
                        })()
                    }
                    exitToMyprofile() {
                        window.location.href = "/myssa/bec-manage-ui/home"
                    }
                    openUseXmlDialog() {
                        this.openDialog = !0
                    }
                    closeUseXmlDialog() {
                        this.openDialog = !1
                    }
                    ngOnDestroy() {
                        this._checkStatementXmlAvailabilitySubscription?.unsubscribe(),
                        this._writeMiWhenLoadingHomePageSubscription?.unsubscribe()
                    }
                    static \u0275fac = function(n) {
                        return new (n || r)(e.\u0275\u0275directiveInject(Ae.hE),e.\u0275\u0275directiveInject(De),e.\u0275\u0275directiveInject(j.HttpClient),e.\u0275\u0275directiveInject(ie),e.\u0275\u0275directiveInject(U.ActivatedRoute))
                    }
                    ;
                    static \u0275cmp = e.\u0275\u0275defineComponent({
                        type: r,
                        selectors: [["app-myssa-statement"]],
                        standalone: !1,
                        decls: 15,
                        vars: 7,
                        consts: [["width", "2-3"], ["id", "statement-ui-body", 1, "statementBody"], [1, "headerBox"], ["variant", "heading2", "font-family", "Segoe UI", "size", "32px"], ["id", "pebes-2"], ["id", "pebes-3-or-6"], ["id", "pebes-4"], ["id", "pebes-5"], ["id", "statement-info-msg", "header", "Important Information", "color", "info", "dismissible", ""], ["font-family", "Segoe UI", "variant", "heading3", "color", "#1b1b1b"], ["fileExtension", "pdf", "mimeType", "application/pdf", "fileNameDescription", "Social Security Statement", "uefLinkType", "pdf", "uefLinkText", "Your Social Security Statement", "size", "16px", 3, "fileFetchFunc", "fileSaveFunc"], ["id", "xml-links", 4, "ngIf"], ["fill", "solid", "type", "submit", "color", "default", "size", "compact", "width", "100px", "height", "37px", 3, "click"], ["id", "xml-links"], ["variant", "heading4", "size", "20px"], ["fileExtension", "xml", "mimeType", "application/xml", "fileNameDescription", "Social Security Statement", "uefLinkType", "download", "uefLinkText", "Download your Statement Data as an XML file", "size", "16px", 3, "fileFetchFunc", "fileSaveFunc"], [3, "linkClick"], [3, "buttonClick", "dialogUpdate", 4, "ngIf"], [3, "buttonClick", "dialogUpdate"], ["header", "We're having problems providing your Statement.", "color", "danger"], ["variant", "body-m", "line-height", "1.5"], ["header", "We're having trouble providing your Statement.", "color", "warning"], ["target", "_blank", 3, "href"], ["header", "There are no earnings on your record.", "color", "info"], ["header", "We cannot provide your Statement at this time.", "color", "warning"], ["aligennment", "left", 1, "leftAlignBulletPointsAdjustment"]],
                        template: function(n, o) {
                            1 & n && (e.\u0275\u0275elementStart(0, "uef-grid")(1, "uef-grid-unit", 0)(2, "div", 1)(3, "div", 2)(4, "p"),
                            e.\u0275\u0275element(5, "br"),
                            e.\u0275\u0275elementStart(6, "uef-typography", 3),
                            e.\u0275\u0275text(7, "Your Social Security Statement "),
                            e.\u0275\u0275elementEnd()()(),
                            e.\u0275\u0275conditionalCreate(8, Le, 21, 3, "uef-container"),
                            e.\u0275\u0275conditionalCreate(9, Fe, 10, 0, "uef-container"),
                            e.\u0275\u0275conditionalCreate(10, Ie, 13, 0, "uef-container"),
                            e.\u0275\u0275conditionalCreate(11, Pe, 14, 1, "uef-container", 4),
                            e.\u0275\u0275conditionalCreate(12, Ue, 14, 1, "uef-container", 5),
                            e.\u0275\u0275conditionalCreate(13, je, 11, 0, "uef-container", 6),
                            e.\u0275\u0275conditionalCreate(14, Be, 18, 0, "uef-container", 7),
                            e.\u0275\u0275elementEnd()()()),
                            2 & n && (e.\u0275\u0275advance(8),
                            e.\u0275\u0275conditional(o.isStatementAvailable && !o.isDownloadingFailed ? 8 : -1),
                            e.\u0275\u0275advance(),
                            e.\u0275\u0275conditional(o.isStatementAvailable && o.isDownloadingFailed ? 9 : -1),
                            e.\u0275\u0275advance(),
                            e.\u0275\u0275conditional(!1 !== o.isStatementAvailable || o.legacyPebesReturnCode ? -1 : 10),
                            e.\u0275\u0275advance(),
                            e.\u0275\u0275conditional(o.isStatementAvailable || "2" !== o.legacyPebesReturnCode ? -1 : 11),
                            e.\u0275\u0275advance(),
                            e.\u0275\u0275conditional(o.isStatementAvailable || "3" !== o.legacyPebesReturnCode && "6" !== o.legacyPebesReturnCode ? -1 : 12),
                            e.\u0275\u0275advance(),
                            e.\u0275\u0275conditional(o.isStatementAvailable || "4" !== o.legacyPebesReturnCode ? -1 : 13),
                            e.\u0275\u0275advance(),
                            e.\u0275\u0275conditional(o.isStatementAvailable || "5" !== o.legacyPebesReturnCode ? -1 : 14))
                        },
                        dependencies: [f.NgIf, S.FileDownloadComponent, S.ContactUsLinkComponent, Re],
                        styles: [".statementBody[_ngcontent-%COMP%]{margin-left:10px}.statementContainerBody[_ngcontent-%COMP%]{background-color:#fff}.headerBox[_ngcontent-%COMP%]{display:flex}.headerBox[_ngcontent-%COMP%]   .header[_ngcontent-%COMP%]{margin-left:5px;color:#363636}.instruction[_ngcontent-%COMP%]{font-size:16px;color:#1b1b1b}.leftAlignBulletPointsAdjustment[_ngcontent-%COMP%]{padding-left:20px;margin-top:0}"]
                    })
                }
                return r
            }
            )()
        }, {
            path: "**",
            redirectTo: "home",
            pathMatch: "full"
        }];
        let He = ( () => {
            class r {
                static \u0275fac = function(n) {
                    return new (n || r)
                }
                ;
                static \u0275mod = e.\u0275\u0275defineNgModule({
                    type: r
                });
                static \u0275inj = e.\u0275\u0275defineInjector({
                    imports: [U.RouterModule.forRoot(Ne), U.RouterModule]
                })
            }
            return r
        }
        )()
          , Ve = ( () => {
            class r {
                _bffApiService;
                _subscription;
                headerInfo;
                uefFooterNavItemList = [];
                constructor(t) {
                    this._bffApiService = t
                }
                ngOnInit() {
                    this._subscription = this._bffApiService.getHeaderV2().subscribe({
                        next: t => {
                            this.headerInfo = {
                                headerData: {
                                    cetPreference: t.headerData.cetPreference,
                                    formattedName: t.headerData.formattedName
                                },
                                homeLink: {
                                    displayText: t.homeLink.displayText,
                                    url: t.homeLink.url
                                },
                                messagesLink: {
                                    displayText: t.messagesLink.displayText,
                                    url: t.messagesLink.url,
                                    messageCount: t.messagesLink.messageCount
                                },
                                signoutLink: {
                                    displayText: t.signoutLink.displayText,
                                    url: t.signoutLink.url
                                },
                                myProfileMenuLinks: [] = t.myProfileMenuLinks,
                                otherLinks: Array.isArray(t?.otherLinks) ? t.otherLinks : []
                            }
                        }
                        ,
                        error: t => {}
                    })
                }
                ngOnDestroy() {
                    this._subscription?.unsubscribe()
                }
                static \u0275fac = function(n) {
                    return new (n || r)(e.\u0275\u0275directiveInject(ie))
                }
                ;
                static \u0275cmp = e.\u0275\u0275defineComponent({
                    type: r,
                    selectors: [["app-root"]],
                    standalone: !1,
                    decls: 2,
                    vars: 2,
                    consts: [[3, "headerInfo", "uefFooterNavItemList"]],
                    template: function(n, o) {
                        1 & n && (e.\u0275\u0275elementStart(0, "myssa-template", 0),
                        e.\u0275\u0275element(1, "router-outlet"),
                        e.\u0275\u0275elementEnd()),
                        2 & n && e.\u0275\u0275property("headerInfo", o.headerInfo)("uefFooterNavItemList", o.uefFooterNavItemList)
                    },
                    dependencies: [S.TemplateComponent, U.RouterOutlet],
                    styles: [".center-vertically[_ngcontent-%COMP%]{display:flex;align-items:center}"]
                })
            }
            return r
        }
        )()
          , Xe = ( () => {
            class r {
                static \u0275fac = function(n) {
                    return new (n || r)
                }
                ;
                static \u0275mod = e.\u0275\u0275defineNgModule({
                    type: r
                });
                static \u0275inj = e.\u0275\u0275defineInjector({
                    providers: [(0,
                    j.provideHttpClient)()],
                    imports: [f.CommonModule, S.MyssaFileModule, S.MyssaUefWrappersModule, S.MyssaContactUsModule]
                })
            }
            return r
        }
        )()
          , We = ( () => {
            class r {
                static \u0275fac = function(n) {
                    return new (n || r)
                }
                ;
                static \u0275mod = e.\u0275\u0275defineNgModule({
                    type: r,
                    bootstrap: [Ve]
                });
                static \u0275inj = e.\u0275\u0275defineInjector({
                    providers: [(0,
                    e.provideBrowserGlobalErrorListeners)(), (0,
                    j.provideHttpClient)(), {
                        provide: S.CONTACT_US_URL,
                        useValue: w_contactUsUrl
                    }, {
                        provide: S.BFF_API_BASE_URL,
                        useValue: "/myssa/myssa-statement-api"
                    }],
                    imports: [we, S.MyssaTemplateModule, He, Xe]
                })
            }
            return r
        }
        )();
        be().bootstrapModule(We, {
            ngZoneEventCoalescing: !0
        }).catch(r => console.error(r)),
        (0,
        Ee.u)(window)
    },
    2578(B, G) {
        var p, e;
        void 0 !== (e = "function" == typeof (p = function() {
            "use strict";
            function b(l, c, _) {
                var u = new XMLHttpRequest;
                u.open("GET", l),
                u.responseType = "blob",
                u.onload = function() {
                    M(u.response, c, _)
                }
                ,
                u.onerror = function() {
                    console.error("could not download file")
                }
                ,
                u.send()
            }
            function T(l) {
                var c = new XMLHttpRequest;
                c.open("HEAD", l, !1);
                try {
                    c.send()
                } catch {}
                return 200 <= c.status && 299 >= c.status
            }
            function C(l) {
                try {
                    l.dispatchEvent(new MouseEvent("click"))
                } catch {
                    var c = document.createEvent("MouseEvents");
                    c.initMouseEvent("click", !0, !0, window, 0, 0, 0, 80, 20, !1, !1, !1, !1, 0, null),
                    l.dispatchEvent(c)
                }
            }
            var g = "object" == typeof window && window.window === window ? window : "object" == typeof self && self.self === self ? self : "object" == typeof global && global.global === global ? global : void 0
              , O = g.navigator && /Macintosh/.test(navigator.userAgent) && /AppleWebKit/.test(navigator.userAgent) && !/Safari/.test(navigator.userAgent)
              , M = g.saveAs || ("object" != typeof window || window !== g ? function() {}
            : "download" in HTMLAnchorElement.prototype && !O ? function(l, c, _) {
                var u = g.URL || g.webkitURL
                  , m = document.createElement("a");
                m.download = c = c || l.name || "download",
                m.rel = "noopener",
                "string" == typeof l ? (m.href = l,
                m.origin === location.origin ? C(m) : T(m.href) ? b(l, c, _) : C(m, m.target = "_blank")) : (m.href = u.createObjectURL(l),
                setTimeout(function() {
                    u.revokeObjectURL(m.href)
                }, 4e4),
                setTimeout(function() {
                    C(m)
                }, 0))
            }
            : "msSaveOrOpenBlob" in navigator ? function(l, c, _) {
                if (c = c || l.name || "download",
                "string" != typeof l)
                    navigator.msSaveOrOpenBlob(function k(l, c) {
                        return typeof c > "u" ? c = {
                            autoBom: !1
                        } : "object" != typeof c && (console.warn("Deprecated: Expected third argument to be a object"),
                        c = {
                            autoBom: !c
                        }),
                        c.autoBom && /^\s*(?:text\/\S*|application\/xml|\S*\/\S*\+xml)\s*;.*charset\s*=\s*utf-8/i.test(l.type) ? new Blob(["\ufeff", l],{
                            type: l.type
                        }) : l
                    }(l, _), c);
                else if (T(l))
                    b(l, c, _);
                else {
                    var u = document.createElement("a");
                    u.href = l,
                    u.target = "_blank",
                    setTimeout(function() {
                        C(u)
                    })
                }
            }
            : function(l, c, _, u) {
                if ((u = u || open("", "_blank")) && (u.document.title = u.document.body.innerText = "downloading..."),
                "string" == typeof l)
                    return b(l, c, _);
                var m = "application/octet-stream" === l.type
                  , N = /constructor/i.test(g.HTMLElement) || g.safari
                  , I = /CriOS\/[\d]+/.test(navigator.userAgent);
                if ((I || m && N || O) && typeof FileReader < "u") {
                    var D = new FileReader;
                    D.onloadend = function() {
                        var x = D.result;
                        x = I ? x : x.replace(/^data:[^;]*;/, "data:attachment/file;"),
                        u ? u.location.href = x : location = x,
                        u = null
                    }
                    ,
                    D.readAsDataURL(l)
                } else {
                    var P = g.URL || g.webkitURL
                      , L = P.createObjectURL(l);
                    u ? u.location = L : location.href = L,
                    u = null,
                    setTimeout(function() {
                        P.revokeObjectURL(L)
                    }, 4e4)
                }
            }
            );
            g.saveAs = M.saveAs = M,
            B.exports = M
        }
        ) ? p.apply(G, []) : p) && (B.exports = e)
    }
}]);
