// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include <wrl/implements.h>

#include "ITerminalProtocol.h"
#include "../TerminalProtocol/ProtocolActivation.h"

using namespace WEX::TestExecution;
namespace Activation = Microsoft::Terminal::Protocol::Activation;

namespace TerminalAppUnitTests
{
    namespace
    {
        struct TestObject : Microsoft::WRL::RuntimeClass<Microsoft::WRL::RuntimeClassFlags<Microsoft::WRL::ClassicCom>, IUnknown>
        {
        };

        struct TestFactory : Microsoft::WRL::RuntimeClass<Microsoft::WRL::RuntimeClassFlags<Microsoft::WRL::ClassicCom>, IClassFactory>
        {
            HRESULT creationResult{ S_OK };

            STDMETHODIMP CreateInstance(IUnknown* outer, REFIID iid, void** result) override
            {
                if (outer)
                {
                    return CLASS_E_NOAGGREGATION;
                }
                if (FAILED(creationResult))
                {
                    return creationResult;
                }
                const auto object = Microsoft::WRL::Make<TestObject>();
                return object ? object.CopyTo(iid, result) : E_OUTOFMEMORY;
            }

            STDMETHODIMP LockServer(BOOL) override
            {
                return S_OK;
            }
        };
    }

    class ProtocolActivationTests
    {
        TEST_CLASS(ProtocolActivationTests);

        TEST_METHOD(RunningFactoryCreatesIndependentObjects);
        TEST_METHOD(RunningFactoryRevokesEndpoint);
        TEST_METHOD(LegacyClassRegistrationStillWorks);
        TEST_METHOD(RegisteredClassTakesPrecedenceOverActiveObject);
        TEST_METHOD(NonFactoryRunningEndpointIsRejected);
        TEST_METHOD(ActivationFailureDoesNotUseRunningFactory);
        TEST_METHOD(MissingProxyFailsExplicitly);
        TEST_METHOD(CoLocatedProxyCreatesProtocolAndCallbackProxies);
    };

    void ProtocolActivationTests::RunningFactoryCreatesIndependentObjects()
    {
        auto apartment = wil::CoInitializeEx(COINIT_MULTITHREADED);
        const auto factory = Microsoft::WRL::Make<TestFactory>();
        Activation::RunningFactoryRegistration registration;
        VERIFY_SUCCEEDED(registration.Initialize(factory.Get()));
        VERIFY_ARE_EQUAL(E_UNEXPECTED, registration.Initialize(factory.Get()));

        wil::com_ptr<IUnknown> first;
        wil::com_ptr<IUnknown> second;
        VERIFY_SUCCEEDED(Activation::CreateInstance(registration.Clsid(), IID_PPV_ARGS(first.put())));
        VERIFY_SUCCEEDED(Activation::CreateInstance(registration.Clsid(), IID_PPV_ARGS(second.put())));
        VERIFY_IS_TRUE(first.get() != second.get());
    }

    void ProtocolActivationTests::RunningFactoryRevokesEndpoint()
    {
        auto apartment = wil::CoInitializeEx(COINIT_MULTITHREADED);
        GUID endpoint{};
        {
            const auto factory = Microsoft::WRL::Make<TestFactory>();
            Activation::RunningFactoryRegistration registration;
            VERIFY_SUCCEEDED(registration.Initialize(factory.Get()));
            endpoint = registration.Clsid();
        }
        wil::com_ptr<IUnknown> object;
        VERIFY_ARE_EQUAL(MK_E_UNAVAILABLE, GetActiveObject(endpoint, nullptr, object.put()));
        VERIFY_ARE_EQUAL(REGDB_E_CLASSNOTREG, Activation::CreateInstance(endpoint, IID_PPV_ARGS(object.put())));
        VERIFY_IS_NULL(object.get());
    }

    void ProtocolActivationTests::LegacyClassRegistrationStillWorks()
    {
        auto apartment = wil::CoInitializeEx(COINIT_MULTITHREADED);
        GUID clsid{};
        VERIFY_SUCCEEDED(CoCreateGuid(&clsid));
        const auto factory = Microsoft::WRL::Make<TestFactory>();
        wil::unique_com_class_object_cookie registration;
        VERIFY_SUCCEEDED(CoRegisterClassObject(clsid, factory.Get(), CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE, registration.put()));
        wil::com_ptr<IUnknown> object;
        VERIFY_SUCCEEDED(Activation::CreateInstance(clsid, IID_PPV_ARGS(object.put())));
    }

    void ProtocolActivationTests::RegisteredClassTakesPrecedenceOverActiveObject()
    {
        auto apartment = wil::CoInitializeEx(COINIT_MULTITHREADED);
        GUID clsid{};
        VERIFY_SUCCEEDED(CoCreateGuid(&clsid));
        const auto otherFactory = Microsoft::WRL::Make<TestFactory>();
        otherFactory->creationResult = E_ACCESSDENIED;
        DWORD cookie{};
        VERIFY_SUCCEEDED(RegisterActiveObject(otherFactory.Get(), clsid, ACTIVEOBJECT_STRONG, &cookie));
        auto revoke = wil::scope_exit([&]() { LOG_IF_FAILED(RevokeActiveObject(cookie, nullptr)); });
        const auto factory = Microsoft::WRL::Make<TestFactory>();
        wil::unique_com_class_object_cookie legacy;
        VERIFY_SUCCEEDED(CoRegisterClassObject(clsid, factory.Get(), CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE, legacy.put()));

        wil::com_ptr<IUnknown> result;
        VERIFY_SUCCEEDED(Activation::CreateInstance(clsid, IID_PPV_ARGS(result.put())));
        VERIFY_IS_NOT_NULL(result.get());
    }

    void ProtocolActivationTests::NonFactoryRunningEndpointIsRejected()
    {
        auto apartment = wil::CoInitializeEx(COINIT_MULTITHREADED);
        GUID clsid{};
        VERIFY_SUCCEEDED(CoCreateGuid(&clsid));
        const auto object = Microsoft::WRL::Make<TestObject>();
        DWORD cookie{};
        VERIFY_SUCCEEDED(RegisterActiveObject(object.Get(), clsid, ACTIVEOBJECT_STRONG, &cookie));
        auto revoke = wil::scope_exit([&]() { LOG_IF_FAILED(RevokeActiveObject(cookie, nullptr)); });

        wil::com_ptr<IUnknown> result;
        VERIFY_ARE_EQUAL(E_NOINTERFACE, Activation::CreateInstance(clsid, IID_PPV_ARGS(result.put())));
        VERIFY_IS_NULL(result.get());
    }

    void ProtocolActivationTests::ActivationFailureDoesNotUseRunningFactory()
    {
        auto apartment = wil::CoInitializeEx(COINIT_MULTITHREADED);
        const auto runningFactory = Microsoft::WRL::Make<TestFactory>();
        Activation::RunningFactoryRegistration running;
        VERIFY_SUCCEEDED(running.Initialize(runningFactory.Get()));
        const auto registeredFactory = Microsoft::WRL::Make<TestFactory>();
        registeredFactory->creationResult = E_ACCESSDENIED;
        wil::unique_com_class_object_cookie legacy;
        VERIFY_SUCCEEDED(CoRegisterClassObject(running.Clsid(), registeredFactory.Get(), CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE, legacy.put()));

        wil::com_ptr<IUnknown> result;
        VERIFY_ARE_EQUAL(E_ACCESSDENIED, Activation::CreateInstance(running.Clsid(), IID_PPV_ARGS(result.put())));
        VERIFY_IS_NULL(result.get());
    }

    void ProtocolActivationTests::MissingProxyFailsExplicitly()
    {
        auto apartment = wil::CoInitializeEx(COINIT_MULTITHREADED);
        GUID uniqueName{};
        VERIFY_SUCCEEDED(CoCreateGuid(&uniqueName));
        const auto executable = wil::GetModuleFileNameW<wil::unique_cotaskmem_string>(nullptr);
        const auto missing = std::filesystem::path{ executable.get() }.parent_path() /
                             (winrt::to_hstring(uniqueName) + L".dll").c_str();
        Activation::ProxyRegistration registration;
        VERIFY_ARE_EQUAL(HRESULT_FROM_WIN32(ERROR_MOD_NOT_FOUND), registration.Initialize({ IID_IUnknown }, missing));
    }

    void ProtocolActivationTests::CoLocatedProxyCreatesProtocolAndCallbackProxies()
    {
        auto apartment = wil::CoInitializeEx(COINIT_MULTITHREADED);
        const auto module = wil::GetModuleFileNameW<wil::unique_cotaskmem_string>(wil::GetModuleInstanceHandle());
        const auto path = std::filesystem::path{ module.get() }.parent_path() / L"OpenConsoleProxy.dll";
        Activation::ProxyRegistration registration;
        VERIFY_SUCCEEDED(registration.Initialize({ __uuidof(ITerminalProtocol), __uuidof(ITerminalProtocolEventSink) }, path));
        for (const auto& iid : { __uuidof(ITerminalProtocol), __uuidof(ITerminalProtocolEventSink) })
        {
            CLSID proxyClsid{};
            VERIFY_SUCCEEDED(CoGetPSClsid(iid, &proxyClsid));
            wil::com_ptr<IPSFactoryBuffer> factory;
            VERIFY_SUCCEEDED(CoGetClassObject(proxyClsid, CLSCTX_INPROC_SERVER, nullptr, IID_PPV_ARGS(factory.put())));
            wil::com_ptr<IRpcProxyBuffer> buffer;
            wil::com_ptr<IUnknown> object;
            VERIFY_SUCCEEDED(factory->CreateProxy(nullptr, iid, buffer.put(), object.put_void()));
        }
    }
}
